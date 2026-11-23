//! The game tick: `Updatable`/`Updatables` (lib/server/updatable.hpp,
//! updatables.cpp) and `ServerApp::advanceTime`'s tick state
//! (server_app.cpp).
//!
//! Reference contracts preserved:
//! - `Updatables` keeps objects grouped by level, "objects that are
//!   registered with a lower level are updated before those with a
//!   higher level"; within a level, registration order;
//! - `add` during a `call` defers placement until the pass is over (the
//!   object waits at the vector tail with a negative handle), and
//!   `call` iterates by the *initial* size -- "if new objects are added
//!   to it while we are iterating, they will not be processed until
//!   next tick. This helps prevent infinite loops";
//! - `remove` during a `call` punches a `NULL` hole and the compaction
//!   runs after the pass; a remove of a not-yet-placed object scans the
//!   pending tail instead;
//! - `add` diagnostics: invalid level -> ERROR + false; already added ->
//!   ERROR + false; a handle that points elsewhere -> CRITICAL (the
//!   port panics, the reference aborts);
//! - `advanceTime` measures the last tick's period, then runs
//!   `onEndOfTick`, increments `time_`, `onStartOfTick`, the
//!   Updatables pass, `onTickProcessingComplete` -- in exactly that
//!   order. The period hook is skipped on the first tick
//!   (`lastAdvanceTime_ == 0`).
//!
//! Deviations (documented): `removalHandle_` is kept in a side table
//! keyed by the object's allocation identity (`Rc::as_ptr`) instead of
//! a friend-reachable member -- the observable placement/removal
//! behaviour is identical. `update` receives the collection (the
//! reference's implementations reach their app through members and
//! globals; this is the same flattening as `doBackgroundTask(mgr)`).
//! `timestamp()` is an injectable stamp source (default: monotonic
//! nanoseconds, `stamps_per_second` 1e9) so tests can drive a virtual
//! clock; the reference reads the CPU stamp counter.

use crate::server_app::ServerApp;
use crate::shutdown::GameTime;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

/// `cstdmf::Updatable`: "This interface should be implemented by any
/// object that wants to receive updates. The CellApp can register
/// objects that implement this interface to receive updates every game
/// tick."
pub trait Updatable {
    /// `update`: "This virtual method is called regularly on every
    /// registered Updatable object."
    fn update(&mut self, updatables: &mut Updatables);
}

/// `Updatables`: the level-partitioned update collection. `level_size_[l]`
/// is an end pointer for level `l` (the number of Updatables at that
/// level or lower); objects added mid-pass wait at the vector tail with
/// a negative handle until placement.
pub struct Updatables {
    /// `objects_`: `None` marks a hole punched by a mid-pass removal.
    objects: Vec<Option<Rc<RefCell<dyn Updatable>>>>,
    /// `removalHandle_` per object, keyed by allocation identity.
    handles: HashMap<usize, i32>,
    /// `levelSize_`.
    level_size: Vec<i32>,
    /// `inUpdate_`.
    in_update: bool,
    /// `deletedObjects_`.
    deleted_objects: usize,
}

impl Updatables {
    /// `Updatables( int numLevels = 2 )`.
    pub fn new(num_levels: usize) -> Self {
        Self {
            objects: Vec::new(),
            handles: HashMap::new(),
            level_size: vec![0; num_levels],
            in_update: false,
            deleted_objects: 0,
        }
    }

    /// `size`.
    pub fn size(&self) -> usize {
        self.objects.len()
    }

    fn key(rc: &Rc<RefCell<dyn Updatable>>) -> usize {
        Rc::as_ptr(rc) as *const u8 as usize
    }

    /// `add`: "Objects that are registered with a lower level are
    /// updated before those with a higher level."
    pub fn add(&mut self, object: Rc<RefCell<dyn Updatable>>, level: i32) -> bool {
        if level < 0 || self.level_size.len() as i32 <= level {
            eprintln!("Updatables::add: Invalid level {level}");
            return false;
        }

        let key = Self::key(&object);

        // Error checking only: a handle that is set must either point at
        // this very object ("Already added.") or the object is in an
        // invalid state (the reference CRITICAL_MSG aborts).
        if let Some(&handle) = self.handles.get(&key)
            && handle != -1
        {
            let pointing_at_self = handle >= 0
                && (handle as usize) < self.objects.len()
                && self.objects[handle as usize]
                    .as_ref()
                    .map(|slot| Self::key(slot) == key)
                    .unwrap_or(false);
            if pointing_at_self {
                eprintln!("Updatables::add: Already added.");
            } else {
                panic!("Updatables::add: Updatable object in invalid state.");
            }
            return false;
        }

        // "Initially, the Updatable is placed at the end of the vector,
        // not in any valid level. The desired level is stored in
        // removalHandle_."
        self.handles.insert(key, -1 - level);
        self.objects.push(Some(object));

        if !self.in_update {
            self.adjust_for_added_object();
        }

        true
    }

    /// `adjustForAddedObject`: "adjusts this collection to remove any
    /// holes and correct the size of the levels" -- walks the new slot
    /// down from the top level to its own, shifting level fronts.
    fn adjust_for_added_object(&mut self) {
        assert!(
            *self.level_size.last().unwrap() < self.objects.len() as i32,
            "MF_ASSERT: levelSize_.back() < objects_.size()"
        );

        let back = *self.level_size.last().unwrap() as usize;
        let object = self.objects[back].clone().unwrap();
        let object_key = Self::key(&object);
        let handle = self.handles[&object_key];
        assert!(handle < 0, "MF_ASSERT: pObject->removalHandle_ < 0");
        let level = -1 - handle;
        let mut hole = back as i32;

        let mut curr_level = self.level_size.len() as i32 - 1;

        // "Each element of levelSize_ stores the number of Updatables at
        // that level or lower. It is like an end pointer for that
        // level."
        self.level_size[curr_level as usize] += 1;

        // "Keep moving the hole to a lower level until the right one is
        // found."
        loop {
            curr_level -= 1;
            if curr_level < level {
                break;
            }
            let level_front = self.level_size[curr_level as usize];
            self.level_size[curr_level as usize] += 1;

            if level_front < hole {
                // "Move the front to the hole (the back). The hole is
                // now at the back of the lower level."
                let moved = self.objects[level_front as usize].clone();
                self.objects[hole as usize] = moved;
                if let Some(moved) = &self.objects[hole as usize] {
                    let moved_key = Self::key(moved);
                    self.handles.insert(moved_key, hole);
                }
            } else {
                assert!(level_front == hole, "MF_ASSERT: levelFront == hole");
            }
            hole = level_front;
        }

        self.objects[hole as usize] = Some(object);
        self.handles.insert(object_key, hole);
    }

    /// `remove`.
    pub fn remove(&mut self, object: &Rc<RefCell<dyn Updatable>>) -> bool {
        let key = Self::key(object);
        let mut hole = self.handles.get(&key).copied().unwrap_or(-1);

        if hole >= 0
            && (hole as usize) < self.objects.len()
            && self.objects[hole as usize]
                .as_ref()
                .map(|slot| Self::key(slot) == key)
                .unwrap_or(false)
        {
            self.handles.insert(key, -1);

            if self.in_update {
                self.objects[hole as usize] = None;
                self.deleted_objects += 1;
            } else {
                // Punch the hole, then move it up through every level to
                // the back of the vector.
                self.objects[hole as usize] = None;

                for level in 0..self.level_size.len() {
                    let level_back = self.level_size[level] - 1;
                    if hole <= level_back {
                        assert!(
                            self.objects[hole as usize].is_none(),
                            "MF_ASSERT: objects_[ hole ] == NULL"
                        );
                        // "Move it to the back of the current level and
                        // then consider it to be the front of the next
                        // level."
                        self.level_size[level] -= 1;
                        self.objects[hole as usize] = self.objects[level_back as usize].clone();
                        if let Some(moved) = &self.objects[hole as usize] {
                            let moved_key = Self::key(moved);
                            self.handles.insert(moved_key, hole);
                        }

                        hole = level_back;

                        // For debugging
                        self.objects[hole as usize] = None;
                    }
                }

                assert!(
                    self.objects.last().map(Option::is_none).unwrap_or(false),
                    "MF_ASSERT: objects_.back() == NULL"
                );
                self.objects.pop();
                assert!(
                    *self.level_size.last().unwrap() == self.objects.len() as i32,
                    "MF_ASSERT: levelSize_.back() == objects_.size()"
                );
            }
        } else {
            // A not-yet-placed object (negative handle) waits at the
            // pending tail; scan it.
            if hole < 0 {
                let mut index = *self.level_size.last().unwrap() as usize;
                while index < self.objects.len() {
                    let matches = self.objects[index]
                        .as_ref()
                        .map(|slot| Self::key(slot) == key)
                        .unwrap_or(false);
                    if matches {
                        self.objects.remove(index);
                        return true;
                    }
                    index += 1;
                }
            }

            eprintln!("Updatables::remove: Updatable was not fully added");
            return false;
        }

        true
    }

    /// `call`: "calls the update method of each object in the
    /// collection."
    pub fn call(&mut self) {
        self.in_update = true;
        // "We go by the initial size of the array, not the current size.
        // This means that if new objects are added to it while we are
        // iterating, they will not be processed until next tick. This
        // helps prevent infinite loops."
        let size = self.objects.len();

        for i in 0..size {
            assert!(self.objects.len() >= size, "MF_ASSERT: size growth");

            let slot = self.objects[i].clone();
            if let Some(object) = slot {
                let key = Self::key(&object);
                assert!(
                    self.handles[&key] == i as i32,
                    "MF_ASSERT: removalHandle_ == i"
                );
                object.borrow_mut().update(self);
                assert!(
                    size <= self.objects.len(),
                    "MF_ASSERT: size shrink mid-pass"
                );
            }
        }

        self.in_update = false;

        // "Adjust any objects newly registered while inUpdate_ was true."
        let num_added = self.objects.len() - size;
        for _ in 0..num_added {
            self.adjust_for_added_object();
        }

        assert!(
            *self.level_size.last().unwrap() == self.objects.len() as i32,
            "MF_ASSERT: levelSize_.back() == objects_.size()"
        );

        if self.deleted_objects > 0 {
            let mut curr_idx: i32 = 0;
            let mut curr_level: i32 = 0;

            // "The objects_ vector is traversed to find all of the
            // holes. For each level, the holes are moved to the end of
            // the level which is then considered as the start of the
            // next level."
            while (curr_level < self.level_size.len() as i32 - 1)
                || (curr_idx < *self.level_size.last().unwrap())
            {
                while curr_idx >= self.level_size[curr_level as usize] {
                    curr_level += 1;
                }

                if self.objects[curr_idx as usize].is_none() {
                    self.level_size[curr_level as usize] -= 1;
                    let level_end = self.level_size[curr_level as usize] as usize;
                    let p_end = self.objects[level_end].clone();
                    self.objects[level_end] = None;
                    if let Some(p_end) = p_end {
                        let p_end_key = Self::key(&p_end);
                        self.handles.insert(p_end_key, curr_idx);
                        self.objects[curr_idx as usize] = Some(p_end);
                    }
                } else {
                    curr_idx += 1;
                }
            }

            assert!(
                self.objects.len() as i32 - self.deleted_objects as i32
                    == *self.level_size.last().unwrap(),
                "MF_ASSERT: size - deletedObjects_ == levelSize_.back()"
            );
            self.objects
                .truncate(*self.level_size.last().unwrap() as usize);
        }

        self.deleted_objects = 0;
    }
}

/// The `ServerApp` tick members (`time_`, `lastAdvanceTime_`,
/// `updatables_`) flattened into an embeddable struct -- the role apps
/// own it the way the C++ originals own the members. The stamp source
/// is injectable for virtual-clock tests.
pub struct TickState {
    /// `time_`.
    time: GameTime,
    /// `lastAdvanceTime_`.
    last_advance_time: u64,
    /// `updatables_`.
    updatables: Updatables,
    /// `ServerAppConfig::updateHertz` -- the global static is not ported
    /// yet, so the hertz travels with the state.
    update_hertz: i32,
    /// `timestamp()`; default reads monotonic nanoseconds.
    now: Box<dyn Fn() -> u64>,
    /// `stampsPerSecond()` for `stampsToSeconds`; 1e9 for the nanosecond
    /// default.
    stamps_per_second: u64,
}

impl TickState {
    /// Construction with the configured hertz and the real stamp clock.
    pub fn new(update_hertz: i32) -> Self {
        Self::with_clock(update_hertz, Box::new(stamp_now))
    }

    /// Construction with an injected `timestamp()` (virtual clock).
    pub fn with_clock(update_hertz: i32, now: Box<dyn Fn() -> u64>) -> Self {
        Self {
            time: 0,
            last_advance_time: 0,
            updatables: Updatables::new(2),
            update_hertz,
            now,
            stamps_per_second: 1_000_000_000,
        }
    }

    /// `time`.
    pub fn time(&self) -> GameTime {
        self.time
    }

    /// `setStartTime`: `onSetStartTime( time_, time )` first, then the
    /// assignment -- the hook still sees the old value.
    pub fn set_start_time(&mut self, app: &mut dyn ServerApp, time: GameTime) {
        app.on_set_start_time(self.time, time);
        self.time = time;
    }

    /// `ServerApp::registerForUpdate`: "adds the input object to the
    /// collection of objects that regularly have their update method
    /// called. Objects that are registered with a lower level are
    /// updated before those with a higher level."
    pub fn register_for_update(&mut self, object: Rc<RefCell<dyn Updatable>>, level: i32) -> bool {
        self.updatables.add(object, level)
    }

    /// `ServerApp::deregisterForUpdate`.
    pub fn deregister_for_update(&mut self, object: &Rc<RefCell<dyn Updatable>>) -> bool {
        self.updatables.remove(object)
    }

    /// The registered count (`Updatables::size`).
    pub fn num_updatables(&self) -> usize {
        self.updatables.size()
    }

    /// `ServerApp::callUpdatables` (the profiler label "callUpdates" is
    /// instrumentation-only).
    pub fn call_updatables(&mut self) {
        self.updatables.call();
    }

    /// `ServerApp::advanceTime`: "This method increments the game time."
    /// The stage order is the baseline this module pins.
    pub fn advance_time(&mut self, app: &mut dyn ServerApp) {
        if self.last_advance_time != 0 {
            let tick_period = stamps_to_seconds(
                (self.now)() - self.last_advance_time,
                self.stamps_per_second,
            );
            app.on_tick_period(tick_period, self.update_hertz);
        }

        self.last_advance_time = (self.now)();

        app.on_end_of_tick();

        self.time += 1;

        app.on_start_of_tick();

        self.call_updatables();

        app.on_tick_processing_complete();
    }

    /// `gameTimeInSeconds`.
    pub fn game_time_in_seconds(&self) -> f64 {
        self.time as f64 / self.update_hertz as f64
    }
}

/// `stampsToSeconds`.
fn stamps_to_seconds(stamps: u64, stamps_per_second: u64) -> f64 {
    stamps as f64 / stamps_per_second as f64
}

/// The default `timestamp()`: monotonic nanoseconds since an arbitrary
/// epoch (the reference reads the CPU stamp counter). Shared with the
/// dispatcher's `realClock` default.
pub fn stamp_now() -> u64 {
    foundation::time::stamp_now()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    type Log = Rc<RefCell<Vec<String>>>;

    /// A tagged updatable; optionally runs a behaviour before recording
    /// the visit.
    struct Tagged {
        tag: &'static str,
        log: Log,
        behavior: Option<Box<dyn FnMut(&mut Updatables)>>,
    }

    impl Tagged {
        fn simple(tag: &'static str, log: &Log) -> Rc<RefCell<Self>> {
            Rc::new(RefCell::new(Self {
                tag,
                log: Rc::clone(log),
                behavior: None,
            }))
        }

        fn behaving(
            tag: &'static str,
            log: &Log,
            behavior: Box<dyn FnMut(&mut Updatables)>,
        ) -> Rc<RefCell<Self>> {
            Rc::new(RefCell::new(Self {
                tag,
                log: Rc::clone(log),
                behavior: Some(behavior),
            }))
        }
    }

    impl Updatable for Tagged {
        fn update(&mut self, updatables: &mut Updatables) {
            if let Some(behavior) = &mut self.behavior {
                behavior(updatables);
            }
            self.log.borrow_mut().push(self.tag.to_string());
        }
    }

    fn boxed(object: Rc<RefCell<Tagged>>) -> Rc<RefCell<dyn Updatable>> {
        object as Rc<RefCell<dyn Updatable>>
    }

    // ---- Updatables ----

    /// Level order: lower levels update first; within a level,
    /// registration order.
    #[test]
    fn lower_levels_update_before_higher_ones() {
        let log = Log::new(RefCell::new(Vec::new()));
        let mut updatables = Updatables::new(3);

        updatables.add(boxed(Tagged::simple("a0", &log)), 0);
        updatables.add(boxed(Tagged::simple("b1", &log)), 1);
        updatables.add(boxed(Tagged::simple("c0", &log)), 0);
        updatables.add(boxed(Tagged::simple("d2", &log)), 2);
        updatables.add(boxed(Tagged::simple("e1", &log)), 1);

        updatables.call();
        assert_eq!(
            *log.borrow(),
            ["a0", "c0", "b1", "e1", "d2"],
            "level 0, then level 1, then level 2; registration order within"
        );
    }

    /// An invalid level (negative or past the count) is rejected with
    /// the reference's ERROR line.
    #[test]
    fn add_rejects_an_invalid_level() {
        let log = Log::new(RefCell::new(Vec::new()));
        let mut updatables = Updatables::new(2);
        let object = Tagged::simple("x", &log);

        assert!(!updatables.add(boxed(Rc::clone(&object)), -1));
        assert!(!updatables.add(boxed(Rc::clone(&object)), 2));
        assert_eq!(updatables.size(), 0, "nothing was added");
        assert!(log.borrow().is_empty());
    }

    /// Adding the same object twice is rejected: "Already added.".
    #[test]
    fn add_rejects_an_already_added_object() {
        let log = Log::new(RefCell::new(Vec::new()));
        let mut updatables = Updatables::new(2);
        let object = Tagged::simple("x", &log);

        assert!(updatables.add(boxed(Rc::clone(&object)), 0));
        assert_eq!(updatables.size(), 1);
        assert!(!updatables.add(boxed(Rc::clone(&object)), 1));
        assert_eq!(updatables.size(), 1, "the duplicate was not added");
        // The original placement still updates.
        updatables.call();
        assert_eq!(*log.borrow(), ["x"]);
    }

    /// An object whose handle points elsewhere is in an invalid state --
    /// the reference CRITICAL_MSG aborts, the port panics with the same
    /// message.
    #[test]
    #[should_panic(expected = "Updatable object in invalid state.")]
    fn add_in_invalid_state_panics() {
        let log = Log::new(RefCell::new(Vec::new()));
        let mut updatables = Updatables::new(2);
        let object = Tagged::simple("x", &log);
        updatables.add(boxed(Rc::clone(&object)), 0);

        // Forge a second object that claims the first one's slot.
        let forged = Tagged::simple("forged", &log);
        let forged_key = Updatables::key(&boxed(Rc::clone(&forged)));
        let slot_key = Updatables::key(&boxed(Rc::clone(&object)));
        updatables
            .handles
            .insert(forged_key, updatables.handles[&slot_key]);

        let _ = updatables.add(boxed(forged), 0);
    }

    /// A removed object stops being updated; the collection shrinks.
    #[test]
    fn remove_takes_the_object_out_of_the_tick() {
        let log = Log::new(RefCell::new(Vec::new()));
        let mut updatables = Updatables::new(2);

        let a = Tagged::simple("a", &log);
        let b = Tagged::simple("b", &log);
        updatables.add(boxed(a), 0);
        updatables.add(boxed(Rc::clone(&b)), 1);
        assert_eq!(updatables.size(), 2);

        assert!(updatables.remove(&boxed(b)));
        assert_eq!(updatables.size(), 1);

        updatables.call();
        assert_eq!(*log.borrow(), ["a"], "only the survivor updates");
    }

    /// Removing a not-yet-placed object scans the pending tail -- an
    /// object added during a pass (by another update) can be removed by
    /// a later update in the same pass and never runs.
    #[test]
    fn remove_of_a_pending_object_scans_the_tail() {
        let log = Log::new(RefCell::new(Vec::new()));
        let mut updatables = Updatables::new(2);

        let ghost = Tagged::simple("ghost", &log);
        let ghost_for_add = Rc::clone(&ghost);
        let ghost_for_remove = Rc::clone(&ghost);
        let removed_ok = Rc::new(Cell::new(false));

        let adder = Tagged::behaving(
            "adder",
            &log,
            Box::new(move |updatables| {
                updatables.add(boxed(Rc::clone(&ghost_for_add)), 1);
            }),
        );
        let remover_flag = Rc::clone(&removed_ok);
        let remover = Tagged::behaving(
            "remover",
            &log,
            Box::new(move |updatables| {
                if updatables.remove(&boxed(Rc::clone(&ghost_for_remove))) {
                    remover_flag.set(true);
                }
            }),
        );

        updatables.add(adder, 0);
        updatables.add(remover, 1);

        updatables.call();
        assert_eq!(
            *log.borrow(),
            ["adder", "remover"],
            "the ghost was pending; its removal scanned the tail"
        );
        assert!(removed_ok.get());
        assert_eq!(updatables.size(), 2, "net growth is zero");
    }

    /// A second remove of the same object finds no valid handle and no
    /// pending tail entry: warning + false.
    #[test]
    fn remove_of_an_unknown_object_warns_and_fails() {
        let log = Log::new(RefCell::new(Vec::new()));
        let mut updatables = Updatables::new(2);
        let object = Tagged::simple("x", &log);
        updatables.add(boxed(Rc::clone(&object)), 0);

        assert!(updatables.remove(&boxed(Rc::clone(&object))));
        assert!(!updatables.remove(&boxed(object)), "already gone");
        assert_eq!(updatables.size(), 0);
    }

    /// Updates added during a pass wait for the next tick (the initial
    /// size guard against infinite loops).
    #[test]
    fn updates_added_during_a_call_wait_for_the_next_tick() {
        let log = Log::new(RefCell::new(Vec::new()));
        let mut updatables = Updatables::new(2);

        let added = Rc::new(Cell::new(false));
        let log_for_newcomer = Rc::clone(&log);
        let added_for_closure = Rc::clone(&added);
        let adder = Tagged::behaving(
            "adder",
            &log,
            Box::new(move |updatables| {
                if !added_for_closure.get() {
                    added_for_closure.set(true);
                    updatables.add(boxed(Tagged::simple("newcomer", &log_for_newcomer)), 1);
                }
            }),
        );
        updatables.add(adder, 0);

        updatables.call();
        assert_eq!(
            *log.borrow(),
            ["adder"],
            "the newcomer waits for the next tick"
        );
        assert_eq!(updatables.size(), 2);

        updatables.call();
        assert_eq!(*log.borrow(), ["adder", "adder", "newcomer"]);
    }

    /// Removals during a pass punch holes; the survivors keep updating
    /// in order and the collection compacts after the pass.
    #[test]
    fn removals_during_a_call_compact_after_the_pass() {
        let log = Log::new(RefCell::new(Vec::new()));
        let mut updatables = Updatables::new(2);

        let first = Tagged::simple("first", &log);
        let third = Tagged::simple("third", &log);
        let done = Rc::new(Cell::new(false));
        let done_for_closure = Rc::clone(&done);
        let third_for_closure = Rc::clone(&third);
        let second = Tagged::behaving(
            "second",
            &log,
            Box::new(move |updatables| {
                if !done_for_closure.get() {
                    done_for_closure.set(true);
                    assert!(updatables.remove(&boxed(Rc::clone(&third_for_closure))));
                }
            }),
        );

        updatables.add(boxed(first), 0);
        updatables.add(boxed(Rc::clone(&second)), 1);
        updatables.add(boxed(Rc::clone(&third)), 1);

        updatables.call();
        assert_eq!(
            *log.borrow(),
            ["first", "second"],
            "the third punched a hole mid-pass and never ran"
        );
        assert_eq!(updatables.size(), 2, "compacted after the pass");

        updatables.call();
        assert_eq!(*log.borrow(), ["first", "second", "first", "second"]);
    }

    /// The reference unit test's simple pass: 21 objects over 3 levels,
    /// every object visits once, visits stay non-decreasing in level.
    #[test]
    fn the_reference_simple_pass_visits_in_level_order() {
        const NUM_TO_TEST: usize = 21;
        const NUM_LEVELS: usize = 3;

        let log = Log::new(RefCell::new(Vec::new()));
        let mut updatables = Updatables::new(NUM_LEVELS);

        // The reference gives MyUpdatable the index `i % NUM_LEVELS` and
        // adds at that level; the SimpleTester requires visits to be
        // non-decreasing in the index.
        for i in 0..NUM_TO_TEST {
            let level = i % NUM_LEVELS;
            let tag: &'static str = Box::leak(format!("{}", i % NUM_LEVELS).into_boxed_str());
            updatables.add(boxed(Tagged::simple(tag, &log)), level as i32);
        }
        assert_eq!(updatables.size(), NUM_TO_TEST);

        updatables.call();
        let visits = log.borrow();
        assert_eq!(visits.len(), NUM_TO_TEST, "numCalls == size");
        let mut last: i32 = -1;
        for tag in visits.iter() {
            let level: i32 = tag.parse().unwrap();
            assert!(last <= level, "visits stay non-decreasing");
            last = level;
        }
    }

    /// The reference unit test's add pass: an alternating tester adds a
    /// duplicate at the visited object's level every second visit --
    /// numCalls == the original size, the collection grows to
    /// `3 * origSize / 2` (21 -> 31), and the added objects wait for
    /// the next tick.
    #[test]
    fn the_reference_add_pass_grows_to_three_halves() {
        const NUM_TO_TEST: usize = 21;
        const NUM_LEVELS: usize = 3;

        let log = Log::new(RefCell::new(Vec::new()));
        let mut updatables = Updatables::new(NUM_LEVELS);

        let should_add = Rc::new(Cell::new(false));
        for i in 0..NUM_TO_TEST {
            let level = i % NUM_LEVELS;
            let should_add = Rc::clone(&should_add);
            let log_for_closure = Rc::clone(&log);
            let log_for_tagged = Rc::clone(&log);
            let adder = Tagged::behaving(
                "adder",
                &log_for_tagged,
                Box::new(move |updatables| {
                    if should_add.get() {
                        updatables.add(
                            boxed(Tagged::simple("added", &log_for_closure)),
                            level as i32,
                        );
                    }
                    should_add.set(!should_add.get());
                }),
            );
            updatables.add(adder, level as i32);
        }

        updatables.call();
        assert_eq!(
            log.borrow().len(),
            NUM_TO_TEST,
            "numCalls == origSize: the added objects wait"
        );
        assert_eq!(updatables.size(), 3 * NUM_TO_TEST / 2, "3 * origSize / 2");
    }

    /// The reference unit test's delete pass: an alternating tester
    /// removes the visited object itself every second visit -- 31
    /// objects, 16 removed, 15 remain (`origSize / 2`), and every
    /// object still records its visit on the way out.
    #[test]
    fn the_reference_delete_pass_halves_the_collection() {
        const NUM_TO_TEST: usize = 31;
        const NUM_LEVELS: usize = 3;

        let log = Log::new(RefCell::new(Vec::new()));
        let mut updatables = Updatables::new(NUM_LEVELS);

        let should_remove = Rc::new(Cell::new(true));
        for i in 0..NUM_TO_TEST {
            let level = i % NUM_LEVELS;
            let should_remove = Rc::clone(&should_remove);
            let self_cell: Rc<RefCell<Option<Rc<RefCell<Tagged>>>>> = Rc::new(RefCell::new(None));
            let cell_for_closure = Rc::clone(&self_cell);
            let tag: &'static str = Box::leak(format!("d{i}").into_boxed_str());
            let object = Tagged::behaving(
                tag,
                &log,
                Box::new(move |updatables| {
                    let should = should_remove.get();
                    should_remove.set(!should);
                    if should {
                        let tagged: Rc<RefCell<Tagged>> =
                            Rc::clone(cell_for_closure.borrow().as_ref().unwrap());
                        let me: Rc<RefCell<dyn Updatable>> = tagged;
                        assert!(updatables.remove(&me));
                    }
                }),
            );
            *self_cell.borrow_mut() = Some(Rc::clone(&object));
            updatables.add(object, level as i32);
        }
        assert_eq!(updatables.size(), NUM_TO_TEST);

        updatables.call();
        assert_eq!(
            log.borrow().len(),
            NUM_TO_TEST,
            "numCalls == origSize: removed objects still record the visit"
        );
        assert_eq!(updatables.size(), NUM_TO_TEST / 2, "origSize / 2");

        // The survivors keep updating.
        updatables.call();
        assert_eq!(log.borrow().len(), NUM_TO_TEST + NUM_TO_TEST / 2);
    }

    // ---- TickState / advanceTime ----

    use crate::server_app::base_on_tick_period;

    /// A hook-recording app: every tick hook appends its stage name.
    struct HookApp {
        log: Log,
        periods: Rc<RefCell<Vec<String>>>,
    }

    impl HookApp {
        fn new(log: &Log, periods: &Rc<RefCell<Vec<String>>>) -> Self {
            Self {
                log: Rc::clone(log),
                periods: Rc::clone(periods),
            }
        }
    }

    impl ServerApp for HookApp {
        fn app_name(&self) -> &str {
            "cellapp"
        }

        fn init(&mut self, _args: &[String]) -> bool {
            true
        }

        fn run(&mut self) -> bool {
            true
        }

        fn on_tick_period(&mut self, tick_period: f64, update_hertz: i32) {
            assert_eq!(update_hertz, 10, "the configured hertz travels");
            self.log.borrow_mut().push("tick-period".to_string());
            self.periods.borrow_mut().push(format!("{tick_period}"));
            // The base warning arm runs on the captured period too.
            base_on_tick_period(tick_period, update_hertz);
        }

        fn on_end_of_tick(&mut self) {
            self.log.borrow_mut().push("end-of-tick".to_string());
        }

        fn on_start_of_tick(&mut self) {
            self.log.borrow_mut().push("start-of-tick".to_string());
        }

        fn on_tick_processing_complete(&mut self) {
            self.log
                .borrow_mut()
                .push("processing-complete".to_string());
        }

        fn on_set_start_time(&mut self, old_time: u32, new_time: u32) {
            self.log
                .borrow_mut()
                .push(format!("set-start-time {old_time} -> {new_time}"));
        }
    }

    /// Acceptance: under a virtual clock the tick stages run in exactly
    /// the reference's order -- period hook (from the second tick on),
    /// onEndOfTick, the time increment, onStartOfTick, the Updatables
    /// pass in level order, onTickProcessingComplete.
    #[test]
    fn the_virtual_clock_pins_the_tick_stage_order() {
        let log = Log::new(RefCell::new(Vec::new()));
        let periods = Rc::new(RefCell::new(Vec::new()));
        let mut app = HookApp::new(&log, &periods);
        // The fixture's plain trait methods behave like the reference's.
        assert_eq!(app.app_name(), "cellapp");
        assert!(app.init(&[]));
        assert!(app.run());

        let clock = Rc::new(Cell::new(0u64));
        let mut state = TickState::with_clock(
            10,
            Box::new({
                let clock = Rc::clone(&clock);
                move || clock.get()
            }),
        );

        // Updatables across two levels: level order inside the pass.
        let upd_log = Rc::clone(&log);
        let make = |tag: &'static str| Tagged::simple(tag, &upd_log);
        state.register_for_update(boxed(make("upd-l0-a")), 0);
        state.register_for_update(boxed(make("upd-l1")), 1);
        state.register_for_update(boxed(make("upd-l0-b")), 0);
        assert_eq!(state.num_updatables(), 3);

        // Tick 1: the period hook is skipped (lastAdvanceTime_ == 0).
        clock.set(1_000_000_000);
        state.advance_time(&mut app);
        assert_eq!(
            *log.borrow(),
            [
                "end-of-tick",
                "start-of-tick",
                "upd-l0-a",
                "upd-l0-b",
                "upd-l1",
                "processing-complete",
            ]
        );
        assert_eq!(state.time(), 1);
        assert!(periods.borrow().is_empty(), "no period on the first tick");

        // Tick 2: 0.3 s elapsed -- the period hook leads the stages.
        clock.set(1_300_000_000);
        state.advance_time(&mut app);
        assert_eq!(
            *log.borrow(),
            [
                "end-of-tick",
                "start-of-tick",
                "upd-l0-a",
                "upd-l0-b",
                "upd-l1",
                "processing-complete",
                "tick-period",
                "end-of-tick",
                "start-of-tick",
                "upd-l0-a",
                "upd-l0-b",
                "upd-l1",
                "processing-complete",
            ]
        );
        assert_eq!(*periods.borrow(), ["0.3"]);
        assert_eq!(state.time(), 2);
    }

    /// The default tick hooks are no-ops; an app that overrides none of
    /// them still drives the whole tick (the base warning arm runs on a
    /// slow period).
    #[test]
    fn default_tick_hooks_drive_a_slow_tick_silently() {
        struct BareTickApp;
        impl ServerApp for BareTickApp {
            fn app_name(&self) -> &str {
                "bareapp"
            }
            fn init(&mut self, _args: &[String]) -> bool {
                true
            }
            fn run(&mut self) -> bool {
                true
            }
        }

        let mut app = BareTickApp;
        assert_eq!(app.app_name(), "bareapp");
        assert!(app.init(&[]));
        assert!(app.run());

        let mut state = TickState::with_clock(10, Box::new(|| 0));
        state.advance_time(&mut app);
        // 0.3 s at 10 Hz -> period * hertz = 3.0 > 2.0: the base
        // warning fires inside the default hook.
        state.advance_time(&mut app); // period 0? now stays 0 -> 0.0
        state.advance_time(&mut app);
        assert_eq!(state.time(), 3);

        // The default onSetStartTime and the default onTickPeriod body
        // run through the base arms.
        state.set_start_time(&mut app, 7);
        app.on_tick_period(0.5, 10);
        assert_eq!(state.time(), 7);
    }

    /// `setStartTime` informs the hook with the old time before the
    /// assignment lands.
    #[test]
    fn set_start_time_informs_before_assigning() {
        let log = Log::new(RefCell::new(Vec::new()));
        let mut app = HookApp::new(&log, &Rc::new(RefCell::new(Vec::new())));
        let mut state = TickState::with_clock(10, Box::new(|| 0));

        state.set_start_time(&mut app, 5);
        assert_eq!(state.time(), 5);
        assert_eq!(*log.borrow(), ["set-start-time 0 -> 5"]);
    }

    /// `gameTimeInSeconds` divides by the configured hertz.
    #[test]
    fn game_time_in_seconds_matches_the_hertz() {
        let mut state = TickState::new(10);
        assert_eq!(state.game_time_in_seconds(), 0.0);

        let mut app = HookApp::new(
            &Log::new(RefCell::new(Vec::new())),
            &Rc::new(RefCell::new(Vec::new())),
        );
        state.set_start_time(&mut app, 15);
        assert!((state.game_time_in_seconds() - 1.5).abs() < 1e-12);
    }

    /// `deregisterForUpdate` through the tick state removes the object
    /// from the pass.
    #[test]
    fn deregister_for_update_takes_the_object_out() {
        let log = Log::new(RefCell::new(Vec::new()));
        let mut app = HookApp::new(&log, &Rc::new(RefCell::new(Vec::new())));
        let mut state = TickState::with_clock(10, Box::new(|| 0));

        let a = Tagged::simple("a", &log);
        let b = Tagged::simple("b", &log);
        state.register_for_update(boxed(a), 0);
        state.register_for_update(boxed(Rc::clone(&b)), 0);
        assert!(state.deregister_for_update(&boxed(b)));

        state.advance_time(&mut app);
        assert_eq!(
            *log.borrow(),
            ["end-of-tick", "start-of-tick", "a", "processing-complete"]
        );
    }

    /// `deregisterForUpdate` outside a pass compacts the level: the
    /// object at the back of the level moves into the hole and its
    /// handle follows it (the reference's level walk).
    #[test]
    fn remove_outside_a_pass_compacts_the_level_and_remaps_the_handle() {
        let log = Log::new(RefCell::new(Vec::new()));
        let mut updatables = Updatables::new(1);

        let a = Tagged::simple("a", &log);
        let b = Tagged::simple("b", &log);
        let c = Tagged::simple("c", &log);
        updatables.add(boxed(a), 0);
        updatables.add(boxed(Rc::clone(&b)), 0);
        updatables.add(boxed(c), 0);
        assert_eq!(updatables.size(), 3);

        // b sits in the middle: its hole is filled from the level back.
        assert!(updatables.remove(&boxed(b)));
        assert_eq!(updatables.size(), 2);

        updatables.call();
        assert_eq!(
            *log.borrow(),
            ["a", "c"],
            "c moved into b's hole and still updates in order"
        );
    }

    /// The pending-tail scan walks past non-matching entries: removing
    /// the SECOND pending object skips the first.
    #[test]
    fn remove_of_the_second_pending_object_skips_the_first() {
        let log = Log::new(RefCell::new(Vec::new()));
        let mut updatables = Updatables::new(2);

        let first = Tagged::simple("first-pending", &log);
        let second = Tagged::simple("second-pending", &log);
        let first_for_add = Rc::clone(&first);
        let second_for_add = Rc::clone(&second);
        let second_for_remove = Rc::clone(&second);
        let removed_ok = Rc::new(Cell::new(false));

        // The adder runs once: a re-add of the removed pending object
        // hits its stale handle and aborts exactly like the reference's
        // CRITICAL check, so pass 2 must not add again.
        let ran = Rc::new(Cell::new(false));
        let ran_for_adder = Rc::clone(&ran);
        let adder = Tagged::behaving(
            "adder",
            &log,
            Box::new(move |updatables| {
                if ran_for_adder.get() {
                    return;
                }
                ran_for_adder.set(true);
                updatables.add(boxed(Rc::clone(&first_for_add)), 1);
                updatables.add(boxed(Rc::clone(&second_for_add)), 1);
            }),
        );
        let remover_flag = Rc::clone(&removed_ok);
        let remover = Tagged::behaving(
            "remover",
            &log,
            Box::new(move |updatables| {
                if updatables.remove(&boxed(Rc::clone(&second_for_remove))) {
                    remover_flag.set(true);
                }
            }),
        );

        updatables.add(adder, 0);
        updatables.add(remover, 1);

        updatables.call();
        assert!(removed_ok.get(), "the second pending object was removed");
        assert_eq!(updatables.size(), 3, "adder + remover + first-pending");

        // The first pending object survived and updates on the next pass
        // (after the recurring adder/remover have run again).
        updatables.call();
        assert_eq!(
            *log.borrow(),
            ["adder", "remover", "adder", "remover", "first-pending"]
        );
    }
}
