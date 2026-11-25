//! The script timer faces of `BigWorld`: the client callback pair and the
//! server timer trio.
//!
//! The reference splits them at compile time: `BigWorld.callback` and
//! `cancelCallback` live behind `#ifndef MF_SERVER`
//! (`lib/pyscript/py_callback.cpp:9`, "Not available on server yet, pending
//! refactoring"), while `addTimer`/`delTimer`/`addFrequentTask` come from
//! `lib/server/` (`app_script_timers.cpp:193`/`:235`,
//! `script_frequent_tasks.cpp:142`), which only server builds link. The port
//! is one crate, so the split becomes a runtime role test over the
//! `componentName` `Script::init` was given: a server component (`base`,
//! `cell`, `database`, `bot`) carries the server trio, anything else with a
//! name (client, editor) carries the callback pair, and an unknown name
//! carries neither -- the same faces, gated the way the builds gate them.
//!
//! Client side (`py_callback.cpp:46-229`): a sorted, stable list of
//! records holding a callable and an empty argument tuple. `tick(timeNow)`
//! walks the list with `gCurrent` parked ON the record being called and
//! erases it only after the call returns (`:103-121`), so a
//! `cancelCallback` from inside the call skips the current record
//! (`:319-326`) and a registration with `delay <= 0` stops its insert scan
//! at `gCurrent` (`:160-171`) -- it always waits a frame, even when the
//! walk is already past every due record. `cancelCallback` erases the
//! first handle match it may touch and stops there (`:327`). Handles come
//! from a counter that wraps at `MAX_TIMER_HANDLE` = 2^32-1
//! (`py_callback.hpp:17-19`, `:202-210`), so they only grow in practice.
//! A failing callback is printed and the walk carries on (`Script::call`,
//! `script.ipp:21-35`).
//!
//! Server side (`cstdmf/time_queue.ipp:157-218` folded with
//! `lib/server/script_timers.cpp` and `app_script_timers.cpp`): a time
//! queue in game ticks at `DEFAULT_GAME_UPDATE_HERTZ` = 10
//! (`lib/server/common.hpp:55`); offsets round to ticks with `+0.5f`, a
//! repeat that rounds below one tick runs every tick, a negative initial
//! offset clamps to zero (`script_timers.cpp:41-83`). `ScriptTimers` maps
//! script ids to queue entries and hands out the smallest free id by
//! linear search (`script_timers.cpp:89-100`) -- ids are reused the moment
//! a timer fires its only shot or is cancelled, because `delTimer` cancels
//! through `TimerHandle::cancel` and `onRelease` frees the id
//! synchronously (`script_timers.cpp:108-124`,
//! `app_script_timers.cpp:68-72`). The queue's `process` loop eats every
//! entry that is due or cancelled (`time_queue.ipp:174-176`); a repeating
//! timer reschedules from its old deadline (`time_ += interval`,
//! `:438-441`), a zero-interval timer self-cancels after one shot
//! (`:434-436`), and a `delTimer` from inside the own callback wins over
//! the reschedule because `triggerTimer` re-checks the cancelled flag
//! (`:429-441`). The id for the call is looked up at call time
//! (`getIDForHandle`, `app_script_timers.cpp:58`) and the callback
//! receives `(id, userArg)` through the `"ik"` format (`:60`), the user
//! arg round-tripped through `intptr`/`uintptr` like the reference casts
//! it. A callable check first, an `onTimer` attribute fallback second
//! (`:105-122`); `delTimer` of an unknown id logs, never raises
//! (`:214-221`).
//!
//! `addFrequentTask` (`script_frequent_tasks.cpp:35-67`) runs its callable
//! every tick across the `Updatables` two ordered levels; a truthy return
//! deregisters the task, a failing call is printed and the task stays.
//! Tasks added during an update wait for the next one -- the walk bounds
//! itself to the size captured before the calls (`updatables.cpp:199-203`)
//! -- and a level outside 0..2 refuses the registration
//! (`updatables.cpp:33-38`), which is the
//! `ValueError "Failed to register for update"` of
//! `script_frequent_tasks.cpp:120`.
//!
//! [`tick`] drives all three walks once per frame, the way the client app
//! ticks its callbacks and the server app processes its time queue; like
//! [`crate::math::tick`] it is a host-app step, not a script surface. The
//! walks release the module lock across every `PyObject_CallObject`
//! because a callback re-enters this module to register and cancel. The
//! reference's `TimerFini` (`app_script_timers.cpp:24-36`) and
//! `Script::clearTimers` (`py_callback.cpp:81-91`) both drain at
//! shutdown, which is [`reset`] here.

use std::collections::BTreeMap;
use std::sync::{Mutex, OnceLock};

use crate::capi;
use crate::pyref::PyRef;

/// The time-queue resolution of the server faces:
/// `DEFAULT_GAME_UPDATE_HERTZ` (`lib/server/common.hpp:55`).
const UPDATE_HERTZ: f64 = 10.0;

/// `MAX_TIMER_HANDLE` (`py_callback.hpp:19`).
const MAX_TIMER_HANDLE: u32 = 0xffff_ffff;

/// One client callback request, the `TimerRecord` of
/// `py_callback.cpp:52-71`.
struct CallbackRecord {
    /// The handle `callback` answered with.
    handle: u32,
    /// The absolute game time the record goes off at.
    time: f64,
    /// The callable; the registration owns its increment.
    function: PyRef,
    /// The fresh empty argument tuple of `py_callback.cpp:274`.
    arguments: PyRef,
}

/// The `gTimers` list (`py_callback.cpp:74-76`) plus the handle counter and
/// the `gCurrent` marker, kept as the handle of the record being called
/// because positions shift under the inserts a callback makes.
struct ClientTimers {
    /// Sorted ascending by time; equal times keep insertion order, which is
    /// what the strict `it->time > t` comparison buys
    /// (`py_callback.cpp:180-184`).
    records: Vec<CallbackRecord>,
    /// `lastTimerHandle_` (`:46`).
    last_handle: u32,
    /// The handle of the record being called, `gCurrent` (`:76`); outside a
    /// call this is `None`, which is the walk's `end()`.
    current: Option<u32>,
}

impl ClientTimers {
    /// `addTimer` (`:172-199`): register at `now + delay`, insert-sorted,
    /// with the scan of a `delay <= 0` registration stopping at the record
    /// being called so it waits a frame.
    fn add(&mut self, function: PyRef, arguments: PyRef, delay: f64, now: f64) -> u32 {
        if self.last_handle == MAX_TIMER_HANDLE {
            // `getHandle` (`:202-210`): the exhaustion log is the only
            // observable, then the counter wraps and keeps going.
            self.last_handle = 0;
        }
        self.last_handle += 1;
        let handle = self.last_handle;
        let time = now + delay;
        let limit = if delay <= 0.0 {
            self.current
                .map(|current| {
                    self.records
                        .iter()
                        .position(|record| record.handle == current)
                        .expect("the record being called stays in the list")
                })
                .unwrap_or(self.records.len())
        } else {
            self.records.len()
        };
        let mut at = limit;
        for (index, record) in self.records[..limit].iter().enumerate() {
            if record.time > time {
                at = index;
                break;
            }
        }
        self.records.insert(
            at,
            CallbackRecord {
                handle,
                time,
                function,
                arguments,
            },
        );
        handle
    }

    /// `py_cancelCallback` (`:304-332`): erase the first handle match,
    /// unless it is the record being called; either way the scan stops
    /// there.
    fn cancel(&mut self, handle: u32) {
        if let Some(position) = self
            .records
            .iter()
            .position(|record| record.handle == handle)
            .filter(|_| self.current != Some(handle))
        {
            self.records.remove(position);
        }
    }

    /// `clearTimers` (`:81-91`): drop every record, releasing the held
    /// references.
    fn clear(&mut self) {
        self.records.clear();
        self.current = None;
    }
}

/// One entry of the server time queue: a `TimeQueueT::Node` folded together
/// with the script id map's payload.
struct TimerNode {
    /// The queue entry's identity, stable across the position shifts a
    /// callback's own `addTimer`/`delTimer` cause mid-fire.
    key: u64,
    /// The absolute deadline in game ticks.
    time: u64,
    /// Ticks between refires; 0 is the one-shot of
    /// `script_timers.cpp:56-65`.
    interval: u64,
    /// The callable (or resolved `onTimer`); the registration owns one
    /// reference (`app_script_timers.cpp:105`).
    function: PyRef,
    /// The `pUser` int (`app_script_timers.cpp:70`).
    user_arg: i32,
    /// The `STATE_CANCELLED` of `time_queue.hpp:44-47`: cancelled entries
    /// wait in the queue until `process` surfaces them
    /// (`time_queue.ipp:174-176`).
    cancelled: bool,
}

/// The server faces' data: the time queue and the `ScriptTimers` id map
/// (`script_timers.cpp:67-73`).
struct ServerTimers {
    /// Kept unsorted; the process step picks the earliest deadline by
    /// scan. The reference's priority queue is a performance shape, and its
    /// order between equal deadlines is heap-dependent -- unspecified, so
    /// scripts must not rely on it and the tests do not.
    queue: Vec<TimerNode>,
    /// script id -> queue entry key.
    map: BTreeMap<i32, u64>,
    /// The next unused queue entry identity.
    next_key: u64,
}

impl ServerTimers {
    /// `ScriptTimersUtil::addTimer` folded with
    /// `ScriptTimers::addTimer` (`script_timers.cpp:41-83`): clamp, round
    /// to ticks, allocate the smallest free script id, queue the entry.
    fn add(&mut self, function: PyRef, initial: f32, repeat: f32, user_arg: i32, now: u64) -> i32 {
        // The clamp of `script_timers.cpp:44-49`; the warning only reaches
        // the log.
        let initial = if initial < 0.0 { 0.0 } else { initial };
        let initial_ticks = now + (initial * UPDATE_HERTZ as f32 + 0.5) as u64;
        let interval = if repeat > 0.0 {
            let ticks = (repeat * UPDATE_HERTZ as f32 + 0.5) as u64;
            // `script_timers.cpp:60-64`: a repeat that rounds below one
            // tick still runs every tick.
            if ticks < 1 { 1 } else { ticks }
        } else {
            0
        };
        // `getNewID` (`script_timers.cpp:89-100`): the linear search makes
        // ids reusable the instant their timer is gone.
        let mut id = 1;
        while self.map.contains_key(&id) {
            id += 1;
        }
        let key = self.next_key;
        self.next_key += 1;
        self.map.insert(id, key);
        self.queue.push(TimerNode {
            key,
            time: initial_ticks,
            interval,
            function,
            user_arg,
            cancelled: false,
        });
        id
    }

    /// `ScriptTimers::delTimer` (`script_timers.cpp:108-124`): cancel the
    /// entry behind the id. The id frees immediately -- the reference's
    /// `TimerHandle::cancel` reaches `onRelease` synchronously -- while the
    /// queue entry itself waits for `process` to surface it.
    fn del(&mut self, id: i32) -> bool {
        let Some(key) = self.map.remove(&id) else {
            return false;
        };
        if let Some(node) = self.queue.iter_mut().find(|node| node.key == key) {
            node.cancelled = true;
        }
        true
    }

    /// `ScriptTimers::cancelAll` (`script_timers.cpp:161-174`), the
    /// shutdown drain: every id frees, every held reference drops.
    fn clear(&mut self) {
        self.queue.clear();
        self.map.clear();
    }
}

/// `ScriptFrequentTask` (`script_frequent_tasks.cpp:35-67`) times two: the
/// `Updatables` two-level registry (`updatables.hpp:14-38`).
struct FrequentTasks {
    /// Level 0 then level 1, in registration order inside a level. A
    /// `None` slot is a task that deregistered itself mid-walk; the slot
    /// stays until it is overwritten or cleared.
    levels: [Vec<Option<PyRef>>; 2],
}

impl FrequentTasks {
    /// `Updatables::add` (`updatables.cpp:30-70`): a level outside 0..2
    /// refuses.
    fn add(&mut self, function: PyRef, level: usize) -> bool {
        let Some(slot) = self.levels.get_mut(level) else {
            return false;
        };
        slot.push(Some(function));
        true
    }

    /// `ScriptFrequentTask::fini` (`script_frequent_tasks.cpp:69-76`), the
    /// shutdown drain.
    fn clear(&mut self) {
        self.levels = [Vec::new(), Vec::new()];
    }
}

/// Everything the module holds between walks.
struct Store {
    /// The client face's records.
    client: ClientTimers,
    /// The server face's queue and id map.
    server: ServerTimers,
    /// The frequent tasks.
    frequent: FrequentTasks,
    /// The client's total game time, `getTotalGameTime`
    /// (`py_callback.cpp:41-44`): the time base `callback` registers
    /// against and the walk compares against.
    game_time: f64,
    /// The server's game tick counter, the `time()` the offsets are added
    /// to (`script_timers.cpp:53`).
    ticks: u64,
    /// The sub-tick remainder the clock accumulates.
    fraction: f64,
}

// SAFETY: the store's `PyRef`s are raw object pointers; the static they
// feed is only ever touched on the interpreter thread that owns the GIL.
unsafe impl Send for Store {}

fn store() -> &'static Mutex<Store> {
    static STORE: OnceLock<Mutex<Store>> = OnceLock::new();
    STORE.get_or_init(|| {
        Mutex::new(Store {
            client: ClientTimers {
                records: Vec::new(),
                last_handle: 0,
                current: None,
            },
            server: ServerTimers {
                queue: Vec::new(),
                map: BTreeMap::new(),
                next_key: 0,
            },
            frequent: FrequentTasks {
                levels: [Vec::new(), Vec::new()],
            },
            game_time: 0.0,
            ticks: 0,
            fraction: 0.0,
        })
    })
}

/// The one lock scope helper: a failed lock is a poisoned mutex, which
/// means a callback panicked; the walks carry on like the reference
/// carries on past a failing callback.
fn guarded() -> std::sync::MutexGuard<'static, Store> {
    store()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Where the client walk stands, the `gCurrent` iterator of
/// `py_callback.cpp:96-135`. The walk is positional, so it survives the
/// position shifts the inserts and cancels a callback cause: the cursor is
/// the handle of the next record to examine.
enum ClientStep {
    /// `gTimers.begin()` -- the walk has not started.
    Start,
    /// Parked on the record with this handle.
    At(u32),
    /// `gTimers.end()` -- the walk finished or hit a not-yet-due record.
    Done,
}

/// The `tick` of `py_callback.cpp:96-135`, one record at a time with the
/// lock released across each call: a callback's own `callback`/
/// `cancelCallback` calls re-enter the module lock. The record being
/// called stays in the list for the duration (`gCurrent` parked on it) and
/// is erased after the call returns.
fn client_tick() {
    let mut step = ClientStep::Start;
    loop {
        let due = {
            let mut store_state = guarded();
            let now = store_state.game_time;
            let index = match step {
                ClientStep::Start => Some(0),
                ClientStep::At(handle) => store_state
                    .client
                    .records
                    .iter()
                    .position(|record| record.handle == handle),
                ClientStep::Done => None,
            };
            let Some(index) = index.filter(|index| {
                *index < store_state.client.records.len()
                    && store_state.client.records[*index].time <= now
            }) else {
                // The reference's else branch: callbacks are in order, so
                // the first not-yet-due record ends the walk
                // (`:122-128`).
                break;
            };
            // Snapshot the call's operands; the entry keeps its own
            // references until the erase after the call.
            let record = &store_state.client.records[index];
            let handle = record.handle;
            let function = record.function.clone();
            let arguments = record.arguments.clone();
            store_state.client.current = Some(handle);
            (handle, function, arguments)
        };
        // `Script::call` (`script.ipp:21-35`): on failure print the
        // traceback and carry on; the source prefix only reaches the log.
        // SAFETY: the record holds live objects and the caller owns the
        // GIL.
        let result = unsafe { capi::PyObject_CallObject(due.1.as_ptr(), due.2.as_ptr()) };
        if result.is_null() {
            // SAFETY: the caller owns the GIL.
            unsafe {
                capi::PyErr_PrintEx(0);
                capi::clear_error();
            }
        } else {
            // SAFETY: the result is a fresh owned reference.
            unsafe { capi::dec_ref(result) };
        }
        // The erase half of the loop body (`:119-121`): the record goes
        // even if the call cancelled it, and the walk advances to
        // whatever the erase left in its place.
        let mut store_state = guarded();
        store_state.client.current = None;
        let position = store_state
            .client
            .records
            .iter()
            .position(|record| record.handle == due.0)
            .expect("the record being called stays in the list");
        store_state.client.records.remove(position);
        step = store_state
            .client
            .records
            .get(position)
            .map_or(ClientStep::Done, |record| ClientStep::At(record.handle));
    }
}

/// The `TimeQueueT::process` loop (`time_queue.ipp:169-218`) folded with
/// the `ScriptTimerHandler::handleTimeout` call
/// (`app_script_timers.cpp:52-66`), one entry at a time with the lock
/// released across each call: a callback's own `addTimer`/`delTimer` calls
/// re-enter the module lock.
fn server_process() {
    loop {
        // The earliest entry, cancelled ones included -- they are dropped
        // when they surface, exactly the reference's loop condition
        // (`time_queue.ipp:174-176`). Ties break by entry age; the
        // reference's heap order between equal deadlines is unspecified
        // and scripts must not rely on it.
        let next = guarded()
            .server
            .queue
            .iter()
            .min_by_key(|node| (node.time, node.key))
            .map(|node| (node.time, node.cancelled, node.key));
        let Some((time, cancelled, key)) = next else {
            return;
        };
        let mut store_state = guarded();
        let now = store_state.ticks;
        if cancelled {
            // The purge half of the loop body (`:206-212`): the id freed
            // when the cancel happened, so only the queue entry goes.
            store_state.server.queue.retain(|node| node.key != key);
            continue;
        }
        if time > now {
            return;
        }
        // `handleTimeout` (`app_script_timers.cpp:52-66`): resolve the
        // current script id for the entry, then call with `(id, userArg)`
        // -- the id looked up at call time, the user arg round-tripped
        // through the same integer widths the reference casts it.
        let node = store_state
            .server
            .queue
            .iter()
            .find(|node| node.key == key)
            .expect("the entry being fired stays in the queue");
        let Some(id) = store_state
            .server
            .map
            .iter()
            .find(|(_, entry_key)| **entry_key == key)
            .map(|(id, _)| *id)
        else {
            continue;
        };
        let function = node.function.clone();
        let user_arg = node.user_arg;
        drop(store_state);
        // The reference fires through one `PyObject_CallFunction`
        // (`app_script_timers.cpp`, `handleTimeout`: `PyObject_CallFunction(
        // pObject, "ik", id, uintptr( pUser ) )`) — the id and user argument
        // cross as two format items and the arguments tuple is built inside
        // the call, instead of the port building two `PyLong`s and a tuple
        // per fire alongside the call itself.
        //
        // SAFETY: the entry holds a live callable, both arguments are C ABI
        // integers ("i" is `c_int`, "k" is `c_ulong`), and the caller owns
        // the GIL.
        let result = unsafe {
            capi::PyObject_CallFunction(
                function.as_ptr(),
                c"ik".as_ptr(),
                id as std::os::raw::c_int,
                user_arg as std::os::raw::c_long as std::os::raw::c_ulong,
            )
        };
        if result.is_null() {
            // `PY_ERROR_CHECK` (`pyobject_plus.hpp:49-56`): print and
            // carry on.
            // SAFETY: the caller owns the GIL.
            unsafe {
                capi::PyErr_PrintEx(0);
                capi::clear_error();
            }
        } else {
            // SAFETY: a fresh owned reference.
            unsafe { capi::dec_ref(result) };
        }
        // Post-call state, re-read by key because the callback could add
        // and cancel entries (`time_queue.ipp:429-441`): cancelled during
        // the call, the one-shot self-cancel (`:434-436`), or the
        // reschedule from the old deadline (`:438-441`).
        let mut store_state = guarded();
        let Some(position) = store_state
            .server
            .queue
            .iter()
            .position(|node| node.key == key)
        else {
            continue;
        };
        let entry = &store_state.server.queue[position];
        if entry.cancelled {
            // Cancelled during its own call: the id freed when the cancel
            // happened, the entry goes now.
            store_state.server.queue.swap_remove(position);
        } else if entry.interval == 0 {
            // The one-shot self-cancel of `triggerTimer`
            // (`time_queue.ipp:434-436`): the id frees the way
            // `onRelease` -> `releaseTimer` frees it
            // (`app_script_timers.cpp:68-72`).
            store_state
                .server
                .map
                .retain(|_, entry_key| *entry_key != key);
            store_state.server.queue.swap_remove(position);
        } else {
            store_state.server.queue[position].time += store_state.server.queue[position].interval;
        }
    }
}

/// The two-level walk of `Updatables::call` (`updatables.cpp:185-207`)
/// with `ScriptFrequentTask::update` (`script_frequent_tasks.cpp:43-63`):
/// each level bounds itself to the size captured before its calls, tasks
/// added during the walk wait for the next tick, a truthy result
/// deregisters, a failing call prints and the task stays.
fn frequent_call() {
    for level in 0..2 {
        let bound = guarded().frequent.levels[level].len();
        for index in 0..bound {
            // Snapshot the callable; a `None` slot is a task that
            // deregistered itself earlier in the walk.
            let function = guarded().frequent.levels[level]
                .get(index)
                .and_then(Option::as_ref)
                .cloned();
            let Some(function) = function else {
                continue;
            };
            // The reference calls through `Script::ask(pCallback.newRef(),
            // PyTuple_New(0))` (`script_frequent_tasks.cpp:44-46`) — an
            // empty tuple allocated, passed and dropped for every task on
            // every tick. The CPython zero-argument fast path reaches the
            // same callable with the same (no) arguments without
            // materializing the tuple; every other contract (a truthy
            // result deregisters, a failing call prints and the task
            // stays) is untouched.
            //
            // SAFETY: the entry holds a live callable and the caller owns
            // the GIL.
            let result = unsafe { capi::PyObject_CallNoArgs(function.as_ptr()) };
            if result.is_null() {
                // `Script::ask` printed (`script.cpp:1208-1226`); the
                // task's own log line only reaches the log, and the task
                // stays registered.
                // SAFETY: the caller owns the GIL.
                unsafe {
                    capi::PyErr_PrintEx(0);
                    capi::clear_error();
                }
            } else {
                // SAFETY: the caller owns the GIL.
                let truthy = unsafe { capi::PyObject_IsTrue(result) } == 1;
                // SAFETY: a fresh owned reference.
                unsafe { capi::dec_ref(result) };
                if truthy {
                    guarded().frequent.levels[level][index] = None;
                }
            }
        }
    }
}

/// `py_callback` (`py_callback.cpp:257-288`): the `"dO"` parse and the
/// callable check share one fixed sentence, the time clamps at zero, and
/// the handle answers as an int.
///
/// # Safety
///
/// A C function call; the interpreter holds the GIL.
unsafe extern "C" fn method_callback(
    _module: *mut capi::PyObject,
    args: *mut capi::PyObject,
) -> *mut capi::PyObject {
    let mut time = 0.0_f64;
    let mut function: *mut capi::PyObject = std::ptr::null_mut();
    // SAFETY: the caller owns the GIL and the parse targets are live
    // locals of C ABI sizes.
    if unsafe { capi::PyArg_ParseTuple(args, c"dO".as_ptr(), &mut time, &mut function) } == 0
        || function.is_null()
        || unsafe { capi::PyCallable_Check(function) } == 0
    {
        // `py_callback.cpp:262-268`: one sentence however the parse went.
        unsafe {
            capi::PyErr_SetString(
                capi::PyExc_TypeError,
                c"BigWorld.callback: Argument parsing error.".as_ptr(),
            );
        }
        return std::ptr::null_mut();
    }
    // `:270`: the clamp.
    let delay = if time < 0.0 { 0.0 } else { time };
    // `:272-274`: the registration increments the function and hands a
    // fresh empty tuple to the record.
    let function_ref = PyRef::borrowed(function).clone();
    // SAFETY: a fresh object this call owns.
    let Some(arguments) = PyRef::from_owned(unsafe { capi::PyTuple_New(0) }) else {
        return std::ptr::null_mut();
    };
    let mut store_state = guarded();
    let now = store_state.game_time;
    let handle = store_state.client.add(function_ref, arguments, delay, now);
    // SAFETY: the caller owns the GIL.
    unsafe { capi::PyLong_FromLong(handle as std::os::raw::c_long) }
}

/// `py_cancelCallback` (`py_callback.cpp:304-333`): the `"i"` parse with
/// its own sentence, the cancel, and `None` however the scan went.
///
/// # Safety
///
/// A C function call; the interpreter holds the GIL.
unsafe extern "C" fn method_cancel_callback(
    _module: *mut capi::PyObject,
    args: *mut capi::PyObject,
) -> *mut capi::PyObject {
    let mut handle = 0_i32;
    // SAFETY: the caller owns the GIL and the parse target is a live
    // local of C ABI size.
    if unsafe { capi::PyArg_ParseTuple(args, c"i".as_ptr(), &mut handle) } == 0 {
        // `py_callback.cpp:308-312`.
        unsafe {
            capi::PyErr_SetString(
                capi::PyExc_TypeError,
                c"py_cancelCallback: Argument parsing error.".as_ptr(),
            );
        }
        return std::ptr::null_mut();
    }
    guarded().client.cancel(handle as u32);
    capi::py_none()
}

/// `py_addTimer` (`app_script_timers.cpp:82-141`): the `"Of|fi"` parse,
/// the `onTimer` fallback for non-callables, the registration, the id.
///
/// # Safety
///
/// A C function call; the interpreter holds the GIL.
unsafe extern "C" fn method_add_timer(
    _module: *mut capi::PyObject,
    args: *mut capi::PyObject,
) -> *mut capi::PyObject {
    let mut object: *mut capi::PyObject = std::ptr::null_mut();
    let mut initial = 0.0_f32;
    let mut repeat = 0.0_f32;
    let mut user_arg = 0_i32;
    // SAFETY: the caller owns the GIL and the parse targets are live
    // locals of C ABI sizes.
    if unsafe {
        capi::PyArg_ParseTuple(
            args,
            c"Of|fi:addTimer".as_ptr(),
            &mut object,
            &mut initial,
            &mut repeat,
            &mut user_arg,
        )
    } == 0
    {
        // The parse failure keeps the parser's own sentence, which is what
        // the reference does (`app_script_timers.cpp:99-103`).
        return std::ptr::null_mut();
    }
    // `:105-122`: the callable check first, the `onTimer` attribute
    // fallback second; a failed attribute fetch is overwritten by the one
    // fixed sentence, which is what `PyErr_SetString` does to whatever
    // `PyObject_GetAttrString` left behind.
    let function = if unsafe { capi::PyCallable_Check(object) } != 0 {
        PyRef::borrowed(object).clone()
    } else {
        // SAFETY: the caller owns the GIL.
        let Some(on_timer) =
            PyRef::from_owned(unsafe { capi::PyObject_GetAttrString(object, c"onTimer".as_ptr()) })
        else {
            // SAFETY: the caller owns the GIL.
            unsafe {
                capi::PyErr_SetString(
                    capi::PyExc_TypeError,
                    c"Callback function is not callable".as_ptr(),
                );
            }
            return std::ptr::null_mut();
        };
        on_timer
    };
    let mut store_state = guarded();
    let now = store_state.ticks;
    let id = store_state
        .server
        .add(function, initial, repeat, user_arg, now);
    // The `id == 0` arm (`:132-138`) cannot fire here: the id search
    // starts at 1 and the queue push cannot fail. The reference reaches it
    // only when the time queue refuses an entry outright.
    // SAFETY: the caller owns the GIL.
    unsafe { capi::PyLong_FromLong(id as std::os::raw::c_long) }
}

/// `py_delTimer` (`app_script_timers.cpp:205-222`): the `"i"` parse keeps
/// the parser's sentence; an unknown id logs and still answers `None`.
///
/// # Safety
///
/// A C function call; the interpreter holds the GIL.
unsafe extern "C" fn method_del_timer(
    _module: *mut capi::PyObject,
    args: *mut capi::PyObject,
) -> *mut capi::PyObject {
    let mut id = 0_i32;
    // SAFETY: the caller owns the GIL and the parse target is a live
    // local of C ABI size.
    if unsafe { capi::PyArg_ParseTuple(args, c"i".as_ptr(), &mut id) } == 0 {
        return std::ptr::null_mut();
    }
    let deleted = guarded().server.del(id);
    // `:214-219`: the failure only logs (`ERROR_MSG`), never raises.
    let _ = deleted;
    capi::py_none()
}

/// `addFrequentTask` (`script_frequent_tasks.cpp:97-128`): the parse is
/// `"O|i"`, the callable check has its own sentence, the answer is the
/// success flag.
///
/// # Safety
///
/// A C function call; the interpreter holds the GIL.
unsafe extern "C" fn method_add_frequent_task(
    _module: *mut capi::PyObject,
    args: *mut capi::PyObject,
) -> *mut capi::PyObject {
    let mut object: *mut capi::PyObject = std::ptr::null_mut();
    let mut level = 0_i32;
    // SAFETY: the caller owns the GIL and the parse targets are live
    // locals of C ABI sizes.
    if unsafe {
        capi::PyArg_ParseTuple(
            args,
            c"O|i:addFrequentTask".as_ptr(),
            &mut object,
            &mut level,
        )
    } == 0
    {
        return std::ptr::null_mut();
    }
    // `:99-104`.
    if unsafe { capi::PyCallable_Check(object) } == 0 {
        // SAFETY: the caller owns the GIL.
        unsafe {
            capi::PyErr_SetString(capi::PyExc_TypeError, c"Argument is not callable".as_ptr());
        }
        return std::ptr::null_mut();
    }
    // The `g_pUpdatables` arm (`:106-111`) cannot fire here: the port
    // builds its registry with the module, so it is never uninitialised.
    let function = PyRef::borrowed(object).clone();
    let registered = guarded().frequent.add(function, level as usize);
    if !registered {
        // `:120`: the invalid-level refusal.
        // SAFETY: the caller owns the GIL.
        unsafe {
            capi::PyErr_SetString(
                capi::PyExc_ValueError,
                c"Failed to register for update".as_ptr(),
            );
        }
        return std::ptr::null_mut();
    }
    // SAFETY: the caller owns the GIL.
    unsafe { capi::PyBool_FromLong(1) }
}

/// A method table's shared handle: `PyMethodDef` carries raw pointers, so
/// the slice alone is not `Sync`, but these tables are immutable constants
/// that `PyModule_AddFunctions` only reads.
struct MethodTable(&'static [capi::PyMethodDef]);
unsafe impl Sync for MethodTable {}

/// The client method table, live under a client (or editor) component.
static CLIENT_FUNCTIONS: MethodTable = MethodTable(&[
    capi::PyMethodDef {
        ml_name: c"callback".as_ptr(),
        ml_meth: Some(method_callback),
        ml_flags: capi::METH_VARARGS,
        ml_doc: std::ptr::null(),
    },
    capi::PyMethodDef {
        ml_name: c"cancelCallback".as_ptr(),
        ml_meth: Some(method_cancel_callback),
        ml_flags: capi::METH_VARARGS,
        ml_doc: std::ptr::null(),
    },
    capi::PyMethodDef::END,
]);

/// The server method table, live under a server component. `addTimer` and
/// `addFrequentTask` are the two faces of the trio with doc strings
/// (`app_script_timers.cpp:193-200`, `script_frequent_tasks.cpp:131-138`).
static SERVER_FUNCTIONS: MethodTable = MethodTable(&[
    capi::PyMethodDef {
        ml_name: c"addTimer".as_ptr(),
        ml_meth: Some(method_add_timer),
        ml_flags: capi::METH_VARARGS,
        ml_doc: c"The function addTimer registers a callback function to be \n\
                  called after \"initialOffset\" seconds. Optionally it can be \
                  repeated every \n\"repeatOffset\" seconds, and have optional \
                  user data (an integer that is \nsimply passed to the timer \
                  function).\n\n\"callback\" must be a function that accepts 2 \
                  arguments; the internal id of the timer \n(useful to remove \
                  the timer via the \"delTimer\" method), and the optional \n\
                  user supplied integer \"userArg\"."
            .as_ptr(),
    },
    capi::PyMethodDef {
        ml_name: c"delTimer".as_ptr(),
        ml_meth: Some(method_del_timer),
        ml_flags: capi::METH_VARARGS,
        ml_doc: std::ptr::null(),
    },
    capi::PyMethodDef {
        ml_name: c"addFrequentTask".as_ptr(),
        ml_meth: Some(method_add_frequent_task),
        ml_flags: capi::METH_VARARGS,
        ml_doc: c"The function addFrequentTask registers a callback function to be \n\
                   called periodically (at game tick frequency). If the callback \
                   ever \nreturns True, it is deregistered and will not be called \
                   again."
            .as_ptr(),
    },
    capi::PyMethodDef::END,
]);

/// Which faces a component carries: the `MF_SERVER` builds (`base`, `cell`,
/// `database`, `bot`) get the server trio, the non-server builds with a
/// name (client, editor) get the callback pair, and an unknown name gets
/// neither -- the manifest's scope rows, applied at install time.
static EMPTY_FUNCTIONS: MethodTable = MethodTable(&[]);

fn carries(component: &str) -> &'static MethodTable {
    match component {
        "base" | "cell" | "database" | "bot" => &SERVER_FUNCTIONS,
        "client" | "editor" => &CLIENT_FUNCTIONS,
        _ => &EMPTY_FUNCTIONS,
    }
}

/// The `PyModuleMethodLink::init` of the timer faces
/// (`script.cpp:1409-1453`): add the component's table to the `BigWorld`
/// module. A table-less component adds nothing, and an add failure logs
/// and carries on in the reference (`ScriptErrorPrint`), so it does here.
///
/// # Safety
///
/// The interpreter must be up, the caller must own the GIL, and `module`
/// must be the live `BigWorld` module object.
pub unsafe fn install(module: *mut capi::PyObject, component: &str) {
    let table = carries(component);
    if table.0.is_empty() {
        return;
    }
    // SAFETY: the module is live and the table is a terminated static
    // array of `PyMethodDef`.
    let added = unsafe { capi::PyModule_AddFunctions(module, table.0.as_ptr()) };
    if added != 0 {
        // The reference logs through `ScriptErrorPrint` and moves on.
        // SAFETY: the caller owns the GIL.
        unsafe {
            capi::PyErr_PrintEx(0);
            capi::clear_error();
        }
    }
}

/// The per-frame step the host app runs: the client callback walk against
/// the accumulated game time, the server time queue against the tick
/// counter, then the frequent tasks. Like [`crate::math::tick`], it is the
/// port's stand-in for the app loops the reference drives these from.
pub fn tick(d_time: f32) {
    {
        let mut store_state = guarded();
        store_state.game_time += d_time as f64;
        // The tick counter advances in whole `1 / UPDATE_HERTZ` steps,
        // accumulating the sub-tick remainder, the way the server's
        // `updateHertz` clock quantises frame time into ticks.
        let advanced = store_state.fraction + d_time as f64 * UPDATE_HERTZ;
        let whole = advanced as u64;
        store_state.fraction = advanced - whole as f64;
        store_state.ticks += whole;
    }
    client_tick();
    server_process();
    frequent_call();
}

/// The shutdown drain of `Script::clearTimers` (`py_callback.cpp:81-91`),
/// `ScriptTimersUtil::cancelAll` (`app_script_timers.cpp:24-36`) and
/// `ScriptFrequentTask::fini` (`script_frequent_tasks.cpp:69-76`): every
/// held reference releases before the interpreter finalises.
pub fn reset() {
    let mut store_state = guarded();
    store_state.client.clear();
    store_state.server.clear();
    store_state.frequent.clear();
    store_state.game_time = 0.0;
    store_state.ticks = 0;
    store_state.fraction = 0.0;
}
