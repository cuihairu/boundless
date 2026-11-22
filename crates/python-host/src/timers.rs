//! The client script timer pair `BigWorld.callback`/`BigWorld.cancelCallback`
//! (`lib/pyscript/py_callback.cpp`, `#ifndef MF_SERVER`, "Not available on
//! server yet, pending refactoring.").
//!
//! A one-shot callback registers a callable to run after a delay; the host's
//! frame tick (`Script::tick`, `py_callback.cpp:96-135`) fires every callback
//! whose time has come, in chronological order, and removes it. The callbacks
//! are the client's timer module: `BigWorld.callback` returns an integer
//! handle, `BigWorld.cancelCallback` removes the pending one, and a cancelled
//! or already-fired callback is simply gone.
//!
//! # Port shape
//!
//! The reference keeps three process globals in `namespace Script`
//! (`py_callback.cpp:21-76`): the `gTimers` list, the `gCurrent` iterator and
//! the `lastTimerHandle_` counter. The port keeps the same three in a
//! [`Timers`] behind a mutex. The reference's single-threaded re-entrancy
//! (a callback may register or cancel during `Script::call`) is preserved: the
//! port releases the mutex across the Python call, so a callback re-enters
//! [`add`]/[`cancel`] exactly as the reference's re-enters `addTimer`/
//! `cancelCallback`.
//!
//! # Deltas
//!
//! - The reference's `TotalGameTimeFn` is engine plumbing
//!   (`setTotalGameTimeFn`/`getTotalGameTime`, `py_callback.cpp:23-44`); the
//!   port keeps the game clock itself ([`set_total_game_time`]) as the
//!   engine's future frame loop, and [`add`] reads it the same way.
//! - `Script::callNextFrame` (`py_callback.cpp:225-229`) is engine-internal
//!   (no Python binding); it is not ported.
//! - `Script::clearTimers` (`py_callback.cpp:81-91`) is called by the app at
//!   a different point than `Script::fini`; the port calls [`clear`] from
//!   `Interpreter::fini` so no record holds a reference into a finalised
//!   interpreter.

use super::capi::{self, PyObject};
use super::pyref::PyRef;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

/// `INVALID_TIMER_HANDLE` (`py_callback.hpp:26`): the "no timer" value.
/// [`Timers::get_handle`] never returns it, so the reference's overflow
/// check is a defensive arm (see [`Timers::add`]).
const INVALID_TIMER_HANDLE: u32 = 0;
/// `MAX_TIMER_HANDLE` (`py_callback.hpp:27`): the counter wraps here.
const MAX_TIMER_HANDLE: u32 = 0xffff_ffff;

/// The game clock, `getTotalGameTime()` (`py_callback.cpp:41-44`). The
/// reference reads it through a registered engine function; the port stores
/// the value the host's frame loop would supply, starting at zero.
static TOTAL_GAME_TIME: AtomicU64 = AtomicU64::new(0.0f64.to_bits());

/// `getTotalGameTime` (`py_callback.cpp:41-44`): the time offsets in
/// [`add`] are measured from.
pub fn total_game_time() -> f64 {
    f64::from_bits(TOTAL_GAME_TIME.load(Ordering::SeqCst))
}

/// The host's game clock, the port of `setTotalGameTimeFn`'s value. The
/// reference registers the engine's frame counter; the port sets it the way
/// the frame loop will, and [`crate::Interpreter`] resets it at init.
pub fn set_total_game_time(time: f64) {
    TOTAL_GAME_TIME.store(time.to_bits(), Ordering::SeqCst);
}

/// One `TimerRecord` (`py_callback.cpp:52-71`): the fire time, the issued
/// handle, and the payload the caller owns (the reference's `function`,
/// `arguments` and `source` fields).
struct Record<T> {
    time: f64,
    handle: u32,
    payload: T,
}

/// What [`Timers::peek_current`] found at the iterator position.
enum Peek<T> {
    /// The record is due: a copy of the payload to run.
    Due { payload: T },
    /// The record is not due yet: the iterator is left at the end, the
    /// reference's `gCurrent = gTimers.end(); break;` (`py_callback.cpp:131`).
    NotDue,
    /// The list is exhausted.
    Done,
}

/// `gTimers`, `gCurrent` and `lastTimerHandle_` (`py_callback.cpp:46-76`).
///
/// The reference's `BW::list` with an iterator is ported as a `Vec` with the
/// iterator as an index; [`Timers::add`] and [`Timers::cancel`] adjust the
/// index the way the linked list's iterator stability would, so the index
/// always names the record the reference's iterator names. `current == len`
/// is "at the end", the reference's `gTimers.end()`.
struct Timers<T> {
    records: Vec<Record<T>>,
    current: usize,
    last_handle: u32,
}

impl<T> Timers<T> {
    const fn new() -> Self {
        Timers {
            records: Vec::new(),
            current: 0,
            last_handle: 0,
        }
    }

    /// `getHandle` (`py_callback.cpp:202-210`): each handle is one more than
    /// the last, wrapping past `MAX_TIMER_HANDLE` to one. Returns whether the
    /// counter wrapped, the reference's `ERROR_MSG` for the port's log sink.
    fn get_handle(&mut self) -> (u32, bool) {
        let wrapped = self.last_handle == MAX_TIMER_HANDLE;
        if wrapped {
            self.last_handle = 0;
        }
        self.last_handle += 1;
        (self.last_handle, wrapped)
    }

    /// `addTimer` (`py_callback.cpp:145-193`): insert-sorts `payload` at
    /// `time` and returns its handle. `delay` keeps the sign that drives the
    /// "must wait at least one frame" rule for a timer registered while its
    /// own tick is running (`py_callback.cpp:155-171`).
    ///
    /// The insertion point is the first record with a time strictly later
    /// than `time`, so equal times keep registration order (the reference's
    /// `it->time > t` test advances past equals).
    fn add(&mut self, time: f64, delay: f64, payload: T) -> u32 {
        let (handle, wrapped) = self.get_handle();
        if wrapped {
            // `ERROR_MSG( "Script::getHandle - timer handles exhausted\n" )`.
            super::output_writer::handle_write(
                "Script::getHandle - timer handles exhausted\n",
                true,
            );
        }
        if handle == INVALID_TIMER_HANDLE {
            // The reference skips the insert on the invalid handle; the arm
            // is unreachable because `get_handle` cannot return it.
            return handle;
        }
        let end = if delay <= 0.0 {
            self.current
        } else {
            self.records.len()
        };
        let mut position = end;
        for (index, record) in self.records[..end].iter().enumerate() {
            if record.time > time {
                position = index;
                break;
            }
        }
        self.records.insert(
            position,
            Record {
                time,
                handle,
                payload,
            },
        );
        // An insert at or before the current record shifts it right; follow
        // it the way the reference's list iterator does.
        if position <= self.current {
            self.current += 1;
        }
        handle
    }

    /// `py_cancelCallback`'s loop (`py_callback.cpp:314-329`): remove the
    /// first record with `handle` and return its payload for the caller to
    /// release; the currently-running record (`gCurrent`) is never erased --
    /// it is removed by [`Timers::finish_current`] after the call instead.
    fn cancel(&mut self, handle: u32) -> Option<T> {
        for (index, record) in self.records.iter().enumerate() {
            if record.handle == handle {
                if index != self.current {
                    if index < self.current {
                        self.current -= 1;
                    }
                    return Some(self.records.remove(index).payload);
                }
                return None;
            }
        }
        None
    }

    /// `Script::tick`'s first line (`py_callback.cpp:98`):
    /// `gCurrent = gTimers.begin()`.
    fn begin(&mut self) {
        self.current = 0;
    }

    /// The tick's test at `py_callback.cpp:102`: is the record at the current
    /// iterator due at `now`? A due record yields a copy of its payload to
    /// run; an undued one leaves the iterator at the end (`:131`).
    fn peek_current(&mut self, now: f64) -> Peek<T>
    where
        T: Clone,
    {
        if self.current >= self.records.len() {
            return Peek::Done;
        }
        let record = &self.records[self.current];
        if record.time <= now {
            Peek::Due {
                payload: record.payload.clone(),
            }
        } else {
            self.current = self.records.len();
            Peek::NotDue
        }
    }

    /// The tick's `gCurrent = gTimers.erase( gCurrent )` (`py_callback.cpp:124`):
    /// drop the record that just ran; the next record slides into the index,
    /// which is why the index does not advance.
    fn finish_current(&mut self) -> T {
        debug_assert!(self.current < self.records.len());
        self.records.remove(self.current).payload
    }

    /// `clearTimers` (`py_callback.cpp:81-91`): release every record; the
    /// payload drops release the reference's function and arguments.
    fn clear(&mut self) {
        self.records.clear();
        self.current = 0;
    }

    /// Whether anything is pending, a test seam.
    #[cfg(test)]
    fn is_empty(&self) -> bool {
        self.records.is_empty()
    }
}

/// The port of a timer record's script-carrying fields: the reference's
/// `function` and `arguments` (`py_callback.cpp:67-68`) and the `reason`
/// string `Script::call` logs (`:69`). The pointers are stored as `usize`
/// the way the wrapper registry stores its capsule pointers: a raw
/// `*mut PyObject` would drag the non-`Send` pointer type into the process
/// static, while the record owns one reference of each pointer.
struct TimerPayload {
    function: usize,
    arguments: usize,
    source: &'static str,
}

impl Clone for TimerPayload {
    /// `Script::call` holds the record's references while it runs; the tick
    /// peeks a copy to call and leaves the record's own references in the
    /// list for [`Timers::finish_current`] to release.
    fn clone(&self) -> Self {
        // SAFETY: the pointers name objects the interpreter handed out and
        // the GIL is held for the whole tick.
        unsafe {
            if !capi::is_finalized() {
                capi::inc_ref(self.function as *mut PyObject);
                capi::inc_ref(self.arguments as *mut PyObject);
            }
        }
        TimerPayload {
            function: self.function,
            arguments: self.arguments,
            source: self.source,
        }
    }
}

impl Drop for TimerPayload {
    /// The reference's `Py_XDECREF` pair in `Script::ask` (`script.cpp:1210-1211`):
    /// the record's own references go when the record leaves the list.
    fn drop(&mut self) {
        if capi::is_finalized() {
            return;
        }
        // SAFETY: the pointers name objects the interpreter handed out and
        // the GIL is held whenever a record is dropped.
        unsafe {
            capi::dec_ref(self.function as *mut PyObject);
            capi::dec_ref(self.arguments as *mut PyObject);
        }
    }
}

/// The process-global timer state, `namespace Script::gTimers` +
/// `gCurrent` + `lastTimerHandle_`.
static TIMERS: Mutex<Timers<TimerPayload>> = Mutex::new(Timers::new());

/// `Script::addTimer` for the Python side: register `function` to fire
/// `delay` seconds from the current game time, with the reference's empty
/// argument tuple. Returns the handle. The handles transfer ownership of
/// their references into the timer record.
pub fn add(function: PyRef, arguments: PyRef, source: &'static str, delay: f64) -> u32 {
    let time = total_game_time() + delay;
    let function_ptr = function.as_ptr() as usize;
    let arguments_ptr = arguments.as_ptr() as usize;
    // The record now owns the references; the handles must not release them.
    std::mem::forget(function);
    std::mem::forget(arguments);
    TIMERS
        .lock()
        .expect("the timer mutex is never poisoned")
        .add(
            time,
            delay,
            TimerPayload {
                function: function_ptr,
                arguments: arguments_ptr,
                source,
            },
        )
}

/// `BigWorld.cancelCallback`: remove the pending timer named by `handle`; a
/// missing or already-fired handle is a no-op (`py_callback.cpp:304-332`).
pub fn cancel(handle: u32) {
    let _ = TIMERS
        .lock()
        .expect("the timer mutex is never poisoned")
        .cancel(handle);
}

/// `Script::tick` (`py_callback.cpp:96-135`): fire every record due at
/// `now`, in order. The mutex is released across each call so a callback may
/// re-enter [`add`]/[`cancel`], the reference's single-threaded re-entrancy.
pub fn tick(now: f64) {
    let mut state = TIMERS.lock().expect("the timer mutex is never poisoned");
    state.begin();
    loop {
        let payload = match state.peek_current(now) {
            Peek::Due { payload, .. } => payload,
            Peek::NotDue | Peek::Done => break,
        };
        drop(state);
        let TimerPayload {
            function,
            arguments,
            source,
        } = payload;
        script_call(
            function as *mut PyObject,
            arguments as *mut PyObject,
            source,
        );
        state = TIMERS.lock().expect("the timer mutex is never poisoned");
        // The record's own references release here, the reference's
        // `Py_XDECREF` pair in `Script::ask` after the call; the peeked copy
        // releases at the end of the iteration.
        let _ = state.finish_current();
    }
}

/// `clearTimers`: release every pending record. The interpreter's fini calls
/// this before `Py_Finalize`; the reference's app does it at its own point
/// (`py_callback.cpp:81-91`).
pub fn clear() {
    TIMERS
        .lock()
        .expect("the timer mutex is never poisoned")
        .clear();
}

/// `Script::ask` as the timer tick uses it (`script.cpp:1148-1262`) with
/// `printException` true: call `function` with `arguments`, print a failed
/// call's traceback, and report success.
///
/// `arguments` is always the reference's empty tuple for these callbacks, so
/// the call is a no-argument one; the non-callable and non-tuple arms are the
/// generic contract of `Script::ask` and are unreachable from the timer path
/// (a record only ever stores what `BigWorld.callback` accepted).
fn script_call(function: *mut PyObject, arguments: *mut PyObject, source: &'static str) -> bool {
    // SAFETY: the interpreter is up (tick runs under the host's GIL) and the
    // handles name objects the module created.
    unsafe {
        // `Script::ask`'s first guard: an error already pending is printed.
        if !capi::PyErr_Occurred().is_null() {
            capi::PyErr_PrintEx(0);
        }
        let mut result: *mut PyObject = std::ptr::null_mut();
        if !function.is_null() && !arguments.is_null() {
            if capi::is_callable(function) && capi::py_tuple_check(arguments) {
                result = call_object(function, arguments);
            } else {
                // `Script::ask` (`script.cpp:1186-1190`): unreachable here.
                capi::PyErr_SetString(
                    capi::PyExc_TypeError,
                    c"Script call attempted on a non-callable object or  pArgs is not a tuple."
                        .as_ptr(),
                );
            }
        } else {
            // `Script::ask` (`script.cpp:1196-1201`): unreachable here; the
            // record always holds both handles.
            capi::PyErr_SetString(
                capi::PyExc_TypeError,
                c"Script call attempted with a NULL function or argument pointers.".as_ptr(),
            );
        }
        if !result.is_null() {
            capi::dec_ref(result);
        }
        if !capi::PyErr_Occurred().is_null() {
            print_timer_error(source);
        }
        !result.is_null()
    }
}

/// `Script::ask`'s `printException` block (`script.cpp:1213-1259`): log
/// `source` plus the exception text the reference composes, then print the
/// traceback and clear.
unsafe fn print_timer_error(source: &str) {
    // SAFETY: called only while the interpreter is up and the GIL is held.
    unsafe {
        let mut ptype: *mut PyObject = std::ptr::null_mut();
        let mut pvalue: *mut PyObject = std::ptr::null_mut();
        let mut ptraceback: *mut PyObject = std::ptr::null_mut();
        capi::PyErr_Fetch(&mut ptype, &mut pvalue, &mut ptraceback);

        // `PyErr_Occurred` returns the exception instance, the same value
        // `Fetch` pulls; the reference composes `str(value) + " (" +
        // str(value) + ")"`, the 3.13 reading of its `pErr` and `pValue`.
        let mut final_error = String::new();
        if !pvalue.is_null() {
            let text = capi::PyObject_Str(pvalue);
            if !text.is_null() {
                final_error.push_str(&capi::to_string(text));
                final_error.push_str(" (");
                capi::dec_ref(text);
            }
        }
        if !ptype.is_null() {
            let text = capi::PyObject_Str(ptype);
            if !text.is_null() {
                final_error.push_str(&capi::to_string(text));
                capi::dec_ref(text);
            }
        }
        if !pvalue.is_null() {
            final_error.push(')');
        }

        super::output_writer::handle_write(&format!("{source} {final_error}\n"), true);

        capi::PyErr_Restore(ptype, pvalue, ptraceback);
        capi::PyErr_PrintEx(0);
        capi::clear_error();
    }
}

/// `PyObject_CallObject(function, arguments)` (`script.cpp:1181`), the port's
/// `PyObject_Vectorcall` form: an empty tuple calls with no arguments, the
/// only shape the timer path uses, and the general case walks the tuple.
unsafe fn call_object(function: *mut PyObject, arguments: *mut PyObject) -> *mut PyObject {
    // SAFETY: the caller guarantees the interpreter is up and the GIL is held.
    unsafe {
        let size = capi::PyTuple_Size(arguments);
        if size < 0 {
            return std::ptr::null_mut();
        }
        if size == 0 {
            return capi::PyObject_CallNoArgs(function);
        }
        let mut items: Vec<*mut PyObject> = Vec::with_capacity(size as usize);
        for index in 0..size {
            items.push(capi::PyTuple_GetItem(arguments, index));
        }
        capi::PyObject_Vectorcall(function, items.as_ptr(), size, std::ptr::null_mut())
    }
}

/// `BigWorld.callback` (`py_callback.cpp:257-288`): register a one-shot
/// callback at `time` and return its handle.
unsafe extern "C" fn py_callback(_module: *mut PyObject, args: *mut PyObject) -> *mut PyObject {
    // SAFETY: called by CPython with the interpreter up and the GIL held.
    unsafe {
        let mut time: f64 = 0.0;
        let mut function: *mut PyObject = std::ptr::null_mut();
        // `PyArg_ParseTuple( args, "dO", &time, &function )` with the
        // callable check (`py_callback.cpp:262-268`).
        if capi::PyArg_ParseTuple(args, c"dO".as_ptr(), &mut time, &mut function) == 0
            || function.is_null()
            || !capi::is_callable(function)
        {
            capi::PyErr_SetString(
                capi::PyExc_TypeError,
                c"BigWorld.callback: Argument parsing error.".as_ptr(),
            );
            return std::ptr::null_mut();
        }
        if time < 0.0 {
            time = 0.0;
        }
        capi::inc_ref(function);
        let empty = PyRef::from_owned(capi::PyTuple_New(0));
        let Some(function) = PyRef::from_owned(function) else {
            return std::ptr::null_mut();
        };
        let Some(arguments) = empty else {
            return std::ptr::null_mut();
        };
        let handle = add(function, arguments, "BigWorld Callback: ", time);
        if handle == INVALID_TIMER_HANDLE {
            // `py_callback.cpp:276-280`: unreachable, see `Timers::add`.
            capi::PyErr_SetString(
                capi::PyExc_TypeError,
                c"py_callback: Callback handle overflow.".as_ptr(),
            );
            return std::ptr::null_mut();
        }
        capi::PyLong_FromLong(handle as std::os::raw::c_long)
    }
}

/// `BigWorld.cancelCallback` (`py_callback.cpp:304-333`): remove the pending
/// callback named by `handle`.
unsafe extern "C" fn py_cancel_callback(
    _module: *mut PyObject,
    args: *mut PyObject,
) -> *mut PyObject {
    // SAFETY: called by CPython with the interpreter up and the GIL held.
    unsafe {
        let mut handle: std::os::raw::c_int = 0;
        if capi::PyArg_ParseTuple(args, c"i".as_ptr(), &mut handle) == 0 {
            capi::PyErr_SetString(
                capi::PyExc_TypeError,
                c"py_cancelCallback: Argument parsing error.".as_ptr(),
            );
            return std::ptr::null_mut();
        }
        cancel(handle as u32);
        capi::py_none()
    }
}

/// The `BigWorld.callback` method descriptor; leaked for the module's
/// lifetime, the reference's static method table.
fn callback_def() -> *mut capi::PyMethodDef {
    Box::leak(Box::new(capi::PyMethodDef {
        ml_name: c"callback".as_ptr() as *const std::ffi::c_char,
        ml_meth: Some(py_callback),
        ml_flags: capi::METH_VARARGS,
        ml_doc: std::ptr::null(),
    }))
}

/// The `BigWorld.cancelCallback` method descriptor; leaked for the module's
/// lifetime, the reference's static method table.
fn cancel_callback_def() -> *mut capi::PyMethodDef {
    Box::leak(Box::new(capi::PyMethodDef {
        ml_name: c"cancelCallback".as_ptr() as *const std::ffi::c_char,
        ml_meth: Some(py_cancel_callback),
        ml_flags: capi::METH_VARARGS,
        ml_doc: std::ptr::null(),
    }))
}

/// Attach `callback` and `cancelCallback` into the `BigWorld` module
/// dictionary, the port of the reference's `PY_MODULE_FUNCTION` links for
/// the client timer pair (`lib/pyscript/py_callback.cpp:288,:333`).
///
/// # Safety
///
/// The interpreter must be up and the caller must own the GIL.
pub unsafe fn register(module: *mut PyObject) {
    // SAFETY: the interpreter is up and the caller owns the GIL.
    unsafe {
        if module.is_null() {
            return;
        }
        let dict = capi::PyModule_GetDict(module);
        let callback = capi::PyCFunction_New(callback_def(), std::ptr::null_mut());
        if !callback.is_null() {
            capi::PyDict_SetItemString(dict, c"callback".as_ptr(), callback);
            capi::dec_ref(callback);
        }
        let cancel = capi::PyCFunction_New(cancel_callback_def(), std::ptr::null_mut());
        if !cancel.is_null() {
            capi::PyDict_SetItemString(dict, c"cancelCallback".as_ptr(), cancel);
            capi::dec_ref(cancel);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tick loop over the core, in the shape `Script::tick` uses, with a
    /// run step the test controls so re-entrancy is exercisable. The payload
    /// is a clone out of the list, so the run step may mutate the state the
    /// way a script callback re-enters `add`/`cancel`.
    fn run_tick(state: &mut Timers<i32>, now: f64, run: &mut dyn FnMut(&mut Timers<i32>, &i32)) {
        state.begin();
        while let Peek::Due { payload } = state.peek_current(now) {
            run(state, &payload);
            let _ = state.finish_current();
        }
    }

    #[test]
    fn handles_count_up_from_one() {
        let mut state = Timers::<i32>::new();
        assert_eq!(state.add(1.0, 1.0, 0), 1);
        assert_eq!(state.add(2.0, 1.0, 1), 2);
        assert_eq!(state.add(3.0, 1.0, 2), 3);
    }

    #[test]
    fn the_counter_wraps_past_the_maximum_and_reports_it() {
        let mut state = Timers::<i32>::new();
        state.last_handle = MAX_TIMER_HANDLE;
        let (handle, wrapped) = state.get_handle();
        assert_eq!(handle, 1);
        assert!(wrapped);
        // The wrap logs one line; the next handle is normal again.
        let (handle, wrapped) = state.get_handle();
        assert_eq!(handle, 2);
        assert!(!wrapped);
    }

    #[test]
    fn an_added_timer_never_gets_the_invalid_handle() {
        let mut state = Timers::<i32>::new();
        assert_ne!(state.add(1.0, 1.0, 0), INVALID_TIMER_HANDLE);
    }

    /// `add` through `getHandle`'s wrap: the counter climbs past `MAX` and
    /// the record is still inserted, with the reference's exhaustion line
    /// handed to the log sink.
    #[test]
    fn the_handle_wrap_is_reported_through_add() {
        let mut state = Timers::<i32>::new();
        state.last_handle = MAX_TIMER_HANDLE;
        let handle = state.add(1.0, 1.0, 0);
        assert_eq!(handle, 1);
        assert_eq!(state.last_handle, 1);
        assert!(!state.is_empty());
    }

    #[test]
    fn timers_fire_in_chronological_order() {
        let mut state = Timers::<i32>::new();
        state.add(3.0, 1.0, 3);
        state.add(1.0, 1.0, 1);
        state.add(2.0, 1.0, 2);
        let mut fired = Vec::new();
        run_tick(&mut state, 10.0, &mut |_, payload| fired.push(*payload));
        assert_eq!(fired, vec![1, 2, 3]);
        assert!(state.is_empty());
    }

    #[test]
    fn equal_times_keep_registration_order() {
        let mut state = Timers::<i32>::new();
        state.add(1.0, 1.0, 0);
        state.add(1.0, 1.0, 1);
        state.add(1.0, 1.0, 2);
        let mut fired = Vec::new();
        run_tick(&mut state, 1.0, &mut |_, payload| fired.push(*payload));
        assert_eq!(fired, vec![0, 1, 2]);
    }

    #[test]
    fn a_timer_not_yet_due_stops_the_tick_at_the_end() {
        let mut state = Timers::<i32>::new();
        state.add(1.0, 1.0, 0);
        state.add(2.0, 1.0, 1);
        state.begin();
        assert!(matches!(state.peek_current(0.5), Peek::NotDue));
        // The iterator was left at the end, so nothing fires.
        let mut fired = Vec::new();
        run_tick(&mut state, 1.5, &mut |_, payload| fired.push(*payload));
        assert_eq!(fired, vec![0]);
    }

    #[test]
    fn cancelling_a_pending_timer_removes_only_it() {
        let mut state = Timers::<i32>::new();
        let a = state.add(1.0, 1.0, 0);
        state.add(2.0, 1.0, 1);
        let _ = state.cancel(a);
        let mut fired = Vec::new();
        run_tick(&mut state, 10.0, &mut |_, payload| fired.push(*payload));
        assert_eq!(fired, vec![1]);
        assert!(state.is_empty());
    }

    #[test]
    fn cancelling_an_unknown_or_fired_handle_is_a_no_op() {
        let mut state = Timers::<i32>::new();
        let a = state.add(1.0, 1.0, 0);
        assert!(state.cancel(99).is_none());
        assert!(state.cancel(a).is_some());
        assert!(state.cancel(a).is_none());
    }

    #[test]
    fn a_zero_delay_timer_registered_during_a_tick_fires_next_tick() {
        let mut state = Timers::<i32>::new();
        let mut fired = Vec::new();
        let mut registered = false;
        // The first timer registers a zero-delay timer while it runs.
        state.add(0.0, 1.0, 0);
        run_tick(&mut state, 10.0, &mut |state, payload| {
            fired.push(*payload);
            if !registered {
                registered = true;
                state.add(10.0, 0.0, 1);
            }
        });
        // Only the first timer fired in this tick.
        assert_eq!(fired, vec![0]);
        // The next tick fires the zero-delay one.
        let mut second = Vec::new();
        run_tick(&mut state, 10.0, &mut |_, payload| second.push(*payload));
        assert_eq!(second, vec![1]);
    }

    /// A positive-delay timer registered while a tick is running lands after
    /// the current record: the iterator does not shift, so the newly added
    /// timer is not picked up by the in-flight tick (the reference's
    /// `it->time > t` insert with a live `gCurrent`).
    #[test]
    fn a_positive_delay_timer_registered_during_a_tick_lands_after_the_current_record() {
        let mut state = Timers::<i32>::new();
        state.add(0.0, 1.0, 0);
        let mut fired = Vec::new();
        run_tick(&mut state, 10.0, &mut |state, payload| {
            fired.push(*payload);
            // Registered while the first record runs: a later deadline.
            state.add(20.0, 1.0, 1);
        });
        // Only the in-flight record fired; the new one waits for a later tick.
        assert_eq!(fired, vec![0]);
        let mut second = Vec::new();
        run_tick(&mut state, 20.0, &mut |_, payload| second.push(*payload));
        assert_eq!(second, vec![1]);
    }

    /// Cancelling a record at an index after the current one during a tick
    /// removes it without shifting the iterator (the reference's
    /// `if (it != gCurrent)` arm with `it` past `gCurrent`).
    #[test]
    fn cancelling_a_later_pending_timer_during_a_tick_keeps_the_run_going() {
        let mut state = Timers::<i32>::new();
        state.add(0.0, 1.0, 0);
        let later = state.add(0.0, 1.0, 1);
        let mut fired = Vec::new();
        run_tick(&mut state, 10.0, &mut |state, payload| {
            if *payload == 0 {
                // The second record is at index 1, past the running index 0.
                assert_eq!(state.cancel(later), Some(1));
            }
            fired.push(*payload);
        });
        assert_eq!(fired, vec![0]);
    }

    #[test]
    fn cancelling_the_running_timer_is_a_no_op_but_it_still_leaves() {
        let mut state = Timers::<i32>::new();
        state.add(0.0, 1.0, 0);
        let mut fired = Vec::new();
        run_tick(&mut state, 10.0, &mut |state, payload| {
            fired.push(*payload);
            // The running record is skipped; the handle matches gCurrent.
            let running = state.records[state.current].handle;
            assert!(state.cancel(running).is_none());
        });
        assert_eq!(fired, vec![0]);
        assert!(state.is_empty());
    }

    #[test]
    fn cancelling_an_earlier_timer_during_a_tick_keeps_the_run_going() {
        let mut state = Timers::<i32>::new();
        let earlier = state.add(0.0, 1.0, 0);
        state.add(0.0, 1.0, 1);
        let mut fired = Vec::new();
        run_tick(&mut state, 10.0, &mut |state, payload| {
            if *payload == 1 {
                // Cancel the already-fired first record: it is gone, so the
                // cancel finds nothing.
                assert!(state.cancel(earlier).is_none());
            }
            fired.push(*payload);
        });
        assert_eq!(fired, vec![0, 1]);
    }

    #[test]
    fn clear_releases_everything() {
        let mut state = Timers::<i32>::new();
        state.add(1.0, 1.0, 0);
        state.add(2.0, 1.0, 1);
        state.clear();
        assert!(state.is_empty());
        let mut fired = Vec::new();
        run_tick(&mut state, 10.0, &mut |_, payload| fired.push(*payload));
        assert!(fired.is_empty());
    }

    #[test]
    fn the_game_time_clock_reads_back_what_it_was_set_to() {
        set_total_game_time(42.5);
        assert_eq!(total_game_time(), 42.5);
    }

    /// `register` guards a null module: there is nothing to attach the
    /// functions to, so the call is a quiet no-op.
    #[test]
    fn registering_into_a_null_module_is_a_no_op() {
        // SAFETY: a null module is the guard's own input; nothing is
        // dereferenced before the early return.
        unsafe { register(std::ptr::null_mut()) };
    }
}
