//! `_bw_profile`: the profiler's Python start/stop face (BW-12-03-02).
//!
//! The reference module is a raw CPython extension
//! (`server/tools/bw_profile/bw_profile.cpp`, `ENABLE_PROFILER` only): its
//! `PyInit__bw_profile` sizes the profiler pool once (128MiB, "TODO: make
//! this configurable"), installs the trace callback and hands out a module
//! of eleven plain C functions. The port is the same face over
//! [`foundation::profiler_frame::ProfilerFrame`]: the method table, the
//! argument handling, the dump-state strings, the error sentences and the
//! trace callback are the reference's.
//!
//! * The table (`bw_profile.cpp:47-73`): the eleven names in order, the
//!   `METH_VARARGS`/`METH_NOARGS` split, and the docstrings — including the
//!   three that carry a trailing space ("Set the Json Dump Count "). The
//!   two `__onThread*` hooks are name-mangled-private to Python the way the
//!   double leading underscore makes them, not gated: Python reaches them.
//! * `profileFunc` (`bw_profile.cpp:76-120`): `PyTrace_CALL`/`RETURN` write
//!   one `EVENT_START`/`EVENT_END` entry per Python frame, named from the
//!   code object's `co_name`, or `"<unknown>"` when the attribute cannot be
//!   read; the `PyTrace_C_*` actions do the same for C functions, gated on
//!   `PyCFunction_Check`. The reference reads the name straight off the
//!   `PyCFunctionObject`'s `m_ml->ml_name`; the port asks the object for
//!   its `__name__`, which for a method object built from a `PyMethodDef`
//!   is the same name without a hand-declared CPython struct layout. The
//!   reference also leaks the strong reference `PyFrame_GetCode` returns
//!   (its cleanup only covers the name object, `:86-95`); the port releases
//!   it — a per-call leak in a long-running host is a behaviour-level
//!   deviation recorded here rather than reproduced.
//! * The import sequence (`bw_profile.cpp:326-346`): pool sizing, the dump
//!   directory defaulting to the executable directory
//!   (`profiler.cpp:1069-1070`), and the trace callback up before the
//!   module object is handed out. `BW_SYSTEMSTAGE_MAIN()` is the stage
//!   system's marker; the stage system is not ported, so the step is
//!   documented here rather than executed.
//! * The dump-state strings (`bw_profile.cpp:259-300`): the four names map
//!   through [`foundation::profiler_frame::JsonDumpState`]'s wire values,
//!   and an unknown name is the exact `RuntimeError` sentence.
//! * `setJsonDumpDir` (`bw_profile.cpp:202-234`): the write probe runs
//!   first (`access(dir, W_OK)`, the `BW_ACCESS`/`BW_W_OK` pair whose
//!   Windows arm is `_access(dir, 2)` — the same value) and a failure is
//!   the exact `IOError` sentence, with the `ERROR_MSG` text dropped the
//!   way a file-less debug build drops it.
//!
//! The JSON dump *output* — the file, its content and the `ACTIVE`/
//! `COMPLETED`/`FAILED` transitions — is BW-12-03-03; this module only
//! raises the request (`startJsonDump`) and reads the state back.

use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_int};
use std::sync::{Mutex, MutexGuard, OnceLock};

use foundation::profiler_frame::{
    Category, EventType, JsonDumpState, MAXIMUM_NUMBER_OF_THREADS, ProfilerFrame,
};

use crate::capi;

/// The module name `PyInit__bw_profile` registers (`bw_profile.cpp:319`).
pub const MODULE_NAME: &CStr = c"_bw_profile";

/// The module docstring (`bw_profile.cpp:320`).
pub const MODULE_DOC: &CStr = c"BigWorld bw_profile module";

/// `MAX_PROFILER_MEMORY_BYTES`, "TODO: make this configurable"
/// (`bw_profile.cpp:332-334`).
pub const MAX_PROFILER_MEMORY_BYTES: usize = 128 * 1024 * 1024;

/// `OurThreadID()` as the frame port pins it: the single port thread is 1
/// (`profiler_frame.rs`'s `add_this_thread`).
const OUR_THREAD_ID: u32 = 1;

/// The name a frame with no readable code object reports
/// (`bw_profile.cpp:90`).
const UNKNOWN_NAME: &str = "<unknown>";

/// One row of the module's method table: the reference's `PyMethodDef`
/// (`bw_profile.cpp:47-73`) with the C trampoline the row dispatches to.
pub struct MethodSpec {
    /// The name Python calls.
    pub name: &'static str,
    /// The C function behind the name.
    pub handler: capi::PyCFunction,
    /// `METH_VARARGS` when true, `METH_NOARGS` otherwise.
    pub varargs: bool,
    /// The docstring, verbatim (three rows carry a trailing space).
    pub doc: &'static str,
}

/// The table, in the reference's order.
pub fn methods() -> &'static [MethodSpec] {
    METHODS
}

const METHODS: &[MethodSpec] = &[
    MethodSpec {
        name: "__onThreadStart",
        handler: bw_profile___onThreadStart,
        varargs: true,
        doc: "Thread start hook; begins profiling a thread",
    },
    MethodSpec {
        name: "__onThreadEnd",
        handler: bw_profile___onThreadEnd,
        varargs: false,
        doc: "Thread end hook; finishes profiling a thread",
    },
    MethodSpec {
        name: "enable",
        handler: bw_profile_enable,
        varargs: true,
        doc: "Enable or disable performance profiling",
    },
    MethodSpec {
        name: "isEnabled",
        handler: bw_profile_isEnabled,
        varargs: true,
        doc: "Returns whether performance profiling currently running",
    },
    MethodSpec {
        name: "tick",
        handler: bw_profile_tick,
        varargs: false,
        doc: "Profiler Tick event",
    },
    MethodSpec {
        name: "startJsonDump",
        handler: bw_profile_startJsonDump,
        varargs: false,
        doc: "Create a json dump file",
    },
    MethodSpec {
        name: "setJsonDumpCount",
        handler: bw_profile_setJsonDumpCount,
        varargs: true,
        doc: "Set the Json Dump Count ",
    },
    MethodSpec {
        name: "setJsonDumpDir",
        handler: bw_profile_setJsonDumpDir,
        varargs: true,
        doc: "Set the Json Dump Dir ",
    },
    MethodSpec {
        name: "getJsonDumpState",
        handler: bw_profile_getJsonDumpState,
        varargs: false,
        doc: "Get the Json Dump State",
    },
    MethodSpec {
        name: "getJsonDumpStateValue",
        handler: bw_profile_getJsonDumpStateValue,
        varargs: true,
        doc: "Get the equivalent dump state enum value of the argument string",
    },
    MethodSpec {
        name: "getJsonOutputFilePath",
        handler: bw_profile_getJsonOutputFilePath,
        varargs: true,
        doc: "Get the latest json output file path ",
    },
];

/// `g_profiler`'s slot — the reference's process-wide singleton. The faces
/// all lock it; the production default (the 128MiB pool) is only
/// constructed when no import has sized it first.
static PROFILER: OnceLock<Mutex<ProfilerFrame>> = OnceLock::new();

/// Locks `g_profiler` for one face call.
fn profiler() -> MutexGuard<'static, ProfilerFrame> {
    PROFILER
        .get_or_init(|| {
            Mutex::new(ProfilerFrame::new(
                MAX_PROFILER_MEMORY_BYTES,
                MAXIMUM_NUMBER_OF_THREADS,
            ))
        })
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// The test seam: seeds the global with a scripted-clock frame *before* any
/// face runs, so the production default (the 128MiB pool) is never
/// constructed inside a unit test.
#[cfg(test)]
fn install_test_profiler(frame: ProfilerFrame) {
    let Some(slot) = PROFILER.get() else {
        let _ = PROFILER.set(Mutex::new(frame));
        return;
    };
    *slot.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = frame;
}

/// Serialises the tests that drive the process-global: the faces run
/// against one `g_profiler`, so their sequences must not interleave.
#[cfg(test)]
fn global_slot() -> MutexGuard<'static, ()> {
    static SLOT: Mutex<()> = Mutex::new(());
    SLOT.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// `PyInit__bw_profile`'s body (`bw_profile.cpp:326-346`) minus the module
/// object creation: the pool is sized once, the dump directory defaults to
/// the executable directory (`profiler.cpp:1069-1070`) and the trace
/// callback goes up before the module is handed out. `BW_SYSTEMSTAGE_MAIN()`
/// marks the main stage first; the stage system is not ported, so that step
/// is this documentation. `false` when the trace callback could not go up
/// (no interpreter): the reference's import only runs inside one.
pub fn init() -> bool {
    // g_profiler.init( MAX_PROFILER_MEMORY_BYTES ) — sizing the pool is the
    // global's construction; the dump-directory default is init's tail
    // (bw_profile.cpp:333-334, profiler.cpp:1069-1070).
    {
        let mut profiler = profiler();
        *profiler = ProfilerFrame::new(MAX_PROFILER_MEMORY_BYTES, MAXIMUM_NUMBER_OF_THREADS);
        profiler.use_default_json_dump_dir();
    }
    // PyEval_SetProfile( &profileFunc, NULL ) (bw_profile.cpp:343).
    unsafe { set_profile_func() }
}

/// `PyEval_SetProfile( &profileFunc, NULL )` (`bw_profile.cpp:133`/`:142`/
/// `:343`), skipped while no interpreter is up.
///
/// # Safety
///
/// Calls into the CPython API; the caller must be inside the interpreter's
/// lifetime.
unsafe fn set_profile_func() -> bool {
    if unsafe { capi::Py_IsInitialized() } == 0 {
        return false;
    }
    // SAFETY: the interpreter is up and `profile_func` is a `Py_tracefunc`.
    unsafe { capi::PyEval_SetProfile(profile_func, std::ptr::null_mut()) };
    true
}

/// Installs the method table onto a live module object: the host-side
/// equivalent of what `PyModule_Create` hands back
/// (`bw_profile.cpp:317-324`, `:336-341`). Names and docstrings are leaked
/// NUL-terminated statics, the way the reference's literals are.
///
/// # Safety
///
/// The module must be a live module object and the caller must hold the GIL.
pub unsafe fn install(module: *mut capi::PyObject) -> bool {
    let mut table: Vec<capi::PyMethodDef> = METHODS
        .iter()
        .map(|method| capi::PyMethodDef {
            ml_name: leak_name(method.name),
            ml_meth: Some(method.handler),
            ml_flags: if method.varargs {
                capi::METH_VARARGS
            } else {
                capi::METH_NOARGS
            },
            ml_doc: leak_name(method.doc),
        })
        .collect();
    table.push(capi::PyMethodDef::END);
    let table: &'static mut [capi::PyMethodDef] = Box::leak(table.into_boxed_slice());
    // SAFETY: the module is live and the table is a terminated static array
    // of `PyMethodDef`.
    let added = unsafe { capi::PyModule_AddFunctions(module, table.as_ptr()) };
    if added != 0 {
        // The reference logs through `ScriptErrorPrint` and moves on.
        // SAFETY: the caller owns the GIL.
        unsafe {
            capi::PyErr_PrintEx(0);
            capi::clear_error();
        }
        return false;
    }
    // PyInit__bw_profile installs the trace callback before handing the
    // module out (bw_profile.cpp:343).
    unsafe { set_profile_func() }
}

/// Leaks one NUL-terminated copy of `text`; the table's strings must outlive
/// every method object.
fn leak_name(text: &str) -> *const c_char {
    match CString::new(text) {
        Ok(encoded) => Box::leak(encoded.into_boxed_c_str()).as_ptr(),
        Err(_) => c"?".as_ptr(),
    }
}

// ---------------------------------------------------------------------------
// The trace callback (`bw_profile.cpp:76-120`).
// ---------------------------------------------------------------------------

/// `profileFunc`: the entry one trace action records — `None` for the
/// actions the switch does not handle. `frame_name` is the entered frame's
/// `co_name` already resolved (the `"<unknown>"` fallback applied);
/// `c_name` is the called C function's name when `PyCFunction_Check`
/// passed, `None` otherwise.
fn trace_entry(
    action: c_int,
    frame_name: Option<&str>,
    c_name: Option<&str>,
) -> Option<(String, EventType)> {
    match action {
        capi::PyTrace_CALL | capi::PyTrace_RETURN => Some((
            frame_name.unwrap_or(UNKNOWN_NAME).to_string(),
            if action == capi::PyTrace_CALL {
                EventType::Start
            } else {
                EventType::End
            },
        )),
        capi::PyTrace_C_CALL => c_name.map(|name| (name.to_string(), EventType::Start)),
        capi::PyTrace_C_RETURN | capi::PyTrace_C_EXCEPTION => {
            c_name.map(|name| (name.to_string(), EventType::End))
        }
        _ => None,
    }
}

/// The entered frame's `co_name`, or `"<unknown>"` when the code object or
/// the attribute cannot be read (`bw_profile.cpp:86-90`).
///
/// # Safety
///
/// `frame` must be the live frame the callback received.
unsafe fn frame_function_name(frame: *mut capi::PyFrameObject) -> String {
    // SAFETY: the frame is the live one CPython passed in.
    let code = unsafe { capi::PyFrame_GetCode(frame) };
    if code.is_null() {
        return UNKNOWN_NAME.to_string();
    }
    // SAFETY: the code object is live for the read.
    let name = unsafe { capi::PyObject_GetAttrString(code, c"co_name".as_ptr()) };
    let text = if name.is_null() {
        UNKNOWN_NAME.to_string()
    } else {
        // SAFETY: the read above.
        let text = unsafe { capi::to_string(name) };
        if text.is_empty() {
            UNKNOWN_NAME.to_string()
        } else {
            text
        }
    };
    // The reference leaks the strong reference `PyFrame_GetCode` returned
    // and only cleans up the name object; the port releases both (the
    // deviation is recorded in the module doc).
    // SAFETY: both handles above are owned.
    unsafe {
        capi::dec_ref(name);
        capi::dec_ref(code.cast());
    }
    text
}

/// The called C function's name when it is a `PyCFunction`, `None` when the
/// gate refuses it (`bw_profile.cpp:99`/`:109`).
///
/// # Safety
///
/// `arg` must be the live argument object the callback received.
unsafe fn c_function_name(arg: *mut capi::PyObject) -> Option<String> {
    // SAFETY: the argument is live.
    if !unsafe { capi::py_c_function_check(arg) } {
        return None;
    }
    // The reference reads `m_ml->ml_name` straight off the struct; the port
    // asks for `__name__` (the same name for a `PyMethodDef`-built method
    // object, without a hand-declared layout).
    // SAFETY: the object is live.
    let name = unsafe { capi::PyObject_GetAttrString(arg, c"__name__".as_ptr()) };
    if name.is_null() {
        // SAFETY: the failed read left the error pending.
        unsafe { capi::clear_error() };
        return Some(UNKNOWN_NAME.to_string());
    }
    // SAFETY: the read above.
    let text = unsafe { capi::to_string(name) };
    // SAFETY: the handle is owned.
    unsafe { capi::dec_ref(name) };
    Some(text)
}

/// `profileFunc` (`bw_profile.cpp:76-120`): always returns 0, whatever the
/// action.
unsafe extern "C" fn profile_func(
    _object: *mut capi::PyObject,
    frame: *mut capi::PyFrameObject,
    action: c_int,
    arg: *mut capi::PyObject,
) -> c_int {
    // SAFETY: the callback contract: the frame and argument are live for the
    // call's duration and the GIL is held.
    let entry = unsafe {
        let frame_name = if frame.is_null() {
            None
        } else {
            Some(frame_function_name(frame))
        };
        let c_name = if arg.is_null() {
            None
        } else {
            c_function_name(arg)
        };
        trace_entry(action, frame_name.as_deref(), c_name.as_deref())
    };
    if let Some((name, event)) = entry {
        profiler().add_entry(&name, event, 0, Category::PYTHON);
    }
    0
}

// ---------------------------------------------------------------------------
// The eleven C functions (`bw_profile.cpp:123-309`). Each trampoline parses
// exactly the reference's format and hands the rest to its face.
// ---------------------------------------------------------------------------

/// `PyArg_ParseTuple( args, "s", &name )`: one string argument.
///
/// # Safety
///
/// The interpreter holds the GIL and `args` is the arguments tuple.
unsafe fn string_face_argument(args: *mut capi::PyObject) -> Option<String> {
    let mut text: *const c_char = std::ptr::null();
    // SAFETY: the format matches the out-parameter list.
    let ok = unsafe { capi::PyArg_ParseTuple(args, c"s".as_ptr(), &mut text) };
    if ok == 0 {
        // The parse failure is CPython's own `TypeError`.
        return None;
    }
    // SAFETY: the parse left a NUL-terminated borrowed pointer.
    Some(
        unsafe { CStr::from_ptr(text) }
            .to_string_lossy()
            .into_owned(),
    )
}

/// `PyArg_ParseTuple( args, "i", &value )`: one int argument.
///
/// # Safety
///
/// The interpreter holds the GIL and `args` is the arguments tuple.
unsafe fn int_face_argument(args: *mut capi::PyObject) -> Option<i32> {
    let mut value: c_int = 0;
    // SAFETY: the format matches the out-parameter list.
    let ok = unsafe { capi::PyArg_ParseTuple(args, c"i".as_ptr(), &mut value) };
    if ok == 0 {
        return None;
    }
    Some(value)
}

/// The C symbols below keep the reference's names (`bw_profile.cpp:123-309`),
/// so each non-snake-case one allows the lint; the module doc carries the
/// mapping.
///
/// `__onThreadStart` (`bw_profile.cpp:123-136`).
#[allow(non_snake_case)]
unsafe extern "C" fn bw_profile___onThreadStart(
    _module: *mut capi::PyObject,
    args: *mut capi::PyObject,
) -> *mut capi::PyObject {
    // SAFETY: the interpreter holds the GIL and `args` is the tuple.
    let Some(name) = (unsafe { string_face_argument(args) }) else {
        return std::ptr::null_mut();
    };
    face_on_thread_start(&name);
    // Py_RETURN_NONE
    capi::py_none()
}

/// `__onThreadEnd` (`bw_profile.cpp:139-145`).
#[allow(non_snake_case)]
unsafe extern "C" fn bw_profile___onThreadEnd(
    _module: *mut capi::PyObject,
    _args: *mut capi::PyObject,
) -> *mut capi::PyObject {
    face_on_thread_end();
    capi::py_none()
}

/// `enable` (`bw_profile.cpp:148-160`): `(i == 0) ? false : true`.
unsafe extern "C" fn bw_profile_enable(
    _module: *mut capi::PyObject,
    args: *mut capi::PyObject,
) -> *mut capi::PyObject {
    // SAFETY: the interpreter holds the GIL and `args` is the tuple.
    let Some(flag) = (unsafe { int_face_argument(args) }) else {
        return std::ptr::null_mut();
    };
    face_enable(flag != 0);
    capi::py_none()
}

/// `isEnabled` (`bw_profile.cpp:163-168`): `watcherEnabled()` as 0/1.
#[allow(non_snake_case)]
unsafe extern "C" fn bw_profile_isEnabled(
    _module: *mut capi::PyObject,
    _args: *mut capi::PyObject,
) -> *mut capi::PyObject {
    // Py_BuildValue( "i", status )
    // SAFETY: the interpreter is up.
    unsafe { capi::PyLong_FromLong(face_is_enabled() as std::os::raw::c_long) }
}

/// `tick` (`bw_profile.cpp:171-176`).
unsafe extern "C" fn bw_profile_tick(
    _module: *mut capi::PyObject,
    _args: *mut capi::PyObject,
) -> *mut capi::PyObject {
    face_tick();
    capi::py_none()
}

/// `startJsonDump` (`bw_profile.cpp:179-184`).
#[allow(non_snake_case)]
unsafe extern "C" fn bw_profile_startJsonDump(
    _module: *mut capi::PyObject,
    _args: *mut capi::PyObject,
) -> *mut capi::PyObject {
    face_start_json_dump();
    capi::py_none()
}

/// `setJsonDumpCount` (`bw_profile.cpp:187-199`).
#[allow(non_snake_case)]
unsafe extern "C" fn bw_profile_setJsonDumpCount(
    _module: *mut capi::PyObject,
    args: *mut capi::PyObject,
) -> *mut capi::PyObject {
    // SAFETY: the interpreter holds the GIL and `args` is the tuple.
    let Some(count) = (unsafe { int_face_argument(args) }) else {
        return std::ptr::null_mut();
    };
    face_set_json_dump_count(count);
    capi::py_none()
}

/// `setJsonDumpDir` (`bw_profile.cpp:202-234`): the write probe first, the
/// exact `IOError` sentence on refusal.
#[allow(non_snake_case)]
unsafe extern "C" fn bw_profile_setJsonDumpDir(
    _module: *mut capi::PyObject,
    args: *mut capi::PyObject,
) -> *mut capi::PyObject {
    // SAFETY: the interpreter holds the GIL and `args` is the tuple.
    let Some(dir) = (unsafe { string_face_argument(args) }) else {
        return std::ptr::null_mut();
    };
    if let Err(error) = face_set_json_dump_dir(&dir) {
        // ERROR_MSG first, then the raised exception (`:219-228`).
        error_msg(&format!(
            "_bw_profile_getJsonDumpStateValue: {}\n",
            error.message(&dir)
        ));
        raise_io_error(&error.exception_message(&dir));
        return std::ptr::null_mut();
    }
    capi::py_none()
}

/// `getJsonDumpState` (`bw_profile.cpp:237-243`).
#[allow(non_snake_case)]
unsafe extern "C" fn bw_profile_getJsonDumpState(
    _module: *mut capi::PyObject,
    _args: *mut capi::PyObject,
) -> *mut capi::PyObject {
    // SAFETY: the interpreter is up.
    unsafe { capi::PyLong_FromLong(face_get_json_dump_state() as std::os::raw::c_long) }
}

/// `getJsonDumpStateValue` (`bw_profile.cpp:259-300`): the string map, an
/// unknown name the exact `RuntimeError` sentence.
#[allow(non_snake_case)]
unsafe extern "C" fn bw_profile_getJsonDumpStateValue(
    _module: *mut capi::PyObject,
    args: *mut capi::PyObject,
) -> *mut capi::PyObject {
    // SAFETY: the interpreter holds the GIL and `args` is the tuple.
    let Some(state) = (unsafe { string_face_argument(args) }) else {
        return std::ptr::null_mut();
    };
    match dump_state_value(&state) {
        Ok(value) => {
            // SAFETY: the interpreter is up.
            unsafe { capi::PyLong_FromLong(value as std::os::raw::c_long) }
        }
        Err(message) => {
            // ERROR_MSG first (`:287-288`), then the raised exception.
            error_msg(&format!(
                "_bw_profile_getJsonDumpStateValue: Unable to get unknown json dump state value {state}"
            ));
            raise_runtime_error(&message);
            std::ptr::null_mut()
        }
    }
}

/// `getJsonOutputFilePath` (`bw_profile.cpp:303-309`): the `args` tuple is
/// accepted and ignored, the way the reference's `METH_VARARGS` signature
/// is.
#[allow(non_snake_case)]
unsafe extern "C" fn bw_profile_getJsonOutputFilePath(
    _module: *mut capi::PyObject,
    _args: *mut capi::PyObject,
) -> *mut capi::PyObject {
    // SAFETY: the interpreter is up.
    unsafe { capi::unicode_from_bytes(face_get_json_output_file_path().as_bytes()) }
}

// ---------------------------------------------------------------------------
// The faces the trampolines hand to, kept interpreter-free so the unit
// tests drive them directly.
// ---------------------------------------------------------------------------

/// `g_profiler.addThisThread( threadName )` + `PyEval_SetProfile`
/// (`bw_profile.cpp:132-133`).
fn face_on_thread_start(name: &str) {
    profiler().add_this_thread(name);
    // The trace install is skipped without an interpreter: the reference
    // runs inside one, the face-level unit tests do not.
    // SAFETY: documented at `set_profile_func`.
    unsafe {
        set_profile_func();
    }
}

/// `g_profiler.removeThread( OurThreadID() )` + the profile uninstall
/// (`bw_profile.cpp:141-142`).
fn face_on_thread_end() {
    profiler().remove_thread(OUR_THREAD_ID);
    // The uninstall mirrors the install: without an interpreter there is
    // nothing to take down, and the reference's `PyEval_SetProfile(NULL,
    // NULL)` only exists inside one.
}

/// `g_profiler.enable( (i == 0) ? false : true )`.
fn face_enable(flag: bool) {
    profiler().enable(flag);
}

/// `watcherEnabled() ? 1 : 0`.
fn face_is_enabled() -> i32 {
    i32::from(profiler().watcher_enabled())
}

/// `g_profiler.tick()`.
fn face_tick() {
    profiler().tick();
}

/// `g_profiler.watcherSetDumpProfile( true )`.
fn face_start_json_dump() {
    profiler().watcher_set_dump_profile(true);
}

/// `g_profiler.setJsonDumpCount( count )`.
fn face_set_json_dump_count(count: i32) {
    profiler().set_json_dump_count(count);
}

/// `setJsonDumpDir`'s gate: the write probe first
/// (`bw_profile.cpp:211`), the store only on a pass (`:231`).
fn face_set_json_dump_dir(dir: &str) -> Result<(), DumpDirError> {
    directory_writable(dir)?;
    profiler().set_json_dump_dir(dir);
    Ok(())
}

/// `g_profiler.getJsonDumpState()` as the module's wire value.
fn face_get_json_dump_state() -> i32 {
    profiler().get_json_dump_state().as_int()
}

/// `g_profiler.getJsonOutputFilePath()`.
fn face_get_json_output_file_path() -> String {
    profiler().get_json_output_file_path()
}

// ---------------------------------------------------------------------------
// The sentences and probes.
// ---------------------------------------------------------------------------

/// The `setJsonDumpDir` refusal: the write probe failed with `strerror`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DumpDirError {
    /// `strerror( errno )` (`bw_profile.cpp:216`).
    pub reason: String,
}

impl DumpDirError {
    /// `errMessage` (`bw_profile.cpp:213-217`): the sentence the `ERROR_MSG`
    /// carries and the raised exception prefixes.
    pub fn message(&self, dir: &str) -> String {
        format!(
            "Unable to write to JSON dump directory: {dir}: {}.",
            self.reason
        )
    }

    /// The exception message (`bw_profile.cpp:223-224`): the function name
    /// prefixed onto `errMessage`.
    pub fn exception_message(&self, dir: &str) -> String {
        format!("setJsonDumpDir: {}", self.message(dir))
    }
}

/// `BW_ACCESS( dir, BW_W_OK )` (`bw_profile.cpp:211`): the write-permission
/// probe. The Windows arm is `_access( dir, 2 )` — the same `W_OK` value.
fn directory_writable(dir: &str) -> Result<(), DumpDirError> {
    let Ok(encoded) = CString::new(dir) else {
        // A path with an interior NUL cannot name a directory.
        return Err(DumpDirError {
            reason: "No such file or directory".to_string(),
        });
    };
    // SAFETY: `encoded` outlives the call.
    let result = unsafe { libc::access(encoded.as_ptr(), libc::W_OK) };
    if result >= 0 {
        return Ok(());
    }
    // `strerror( errno )` (`bw_profile.cpp:212`), verbatim: the OS error's
    // own rendering, not Rust's `os error 2` suffix.
    let code = std::io::Error::last_os_error()
        .raw_os_error()
        .unwrap_or(libc::EIO);
    // SAFETY: `strerror` always returns a valid C string.
    let reason = unsafe { CStr::from_ptr(libc::strerror(code)) }
        .to_string_lossy()
        .into_owned();
    Err(DumpDirError { reason })
}

/// `getJsonDumpStateValue`'s string map (`bw_profile.cpp:269-297`): the
/// four names through the wire values the frame's `JsonDumpState` carries.
/// `Err` is the `RuntimeError` sentence for an unknown name.
pub fn dump_state_value(state: &str) -> Result<i32, String> {
    match state {
        "JSON_DUMPING_INACTIVE" => Ok(JsonDumpState::Inactive.as_int()),
        "JSON_DUMPING_ACTIVE" => Ok(JsonDumpState::Active.as_int()),
        "JSON_DUMPING_COMPLETED" => Ok(JsonDumpState::Completed.as_int()),
        "JSON_DUMPING_FAILED" => Ok(JsonDumpState::Failed.as_int()),
        _ => Err(format!(
            "getJsonDumpStateValue: Invalid dump state value {state}"
        )),
    }
}

/// `ERROR_MSG`: the debug output face. This port keeps no debug sink here,
/// so the message is dropped like a file-less build drops it; the sentence
/// shapes stay in the callers for the record.
fn error_msg(_message: &str) {}

/// `PyErr_SetString( PyExc_IOError, ... )` (`bw_profile.cpp:226`).
fn raise_io_error(message: &str) {
    // SAFETY: the interpreter holds the GIL and the message outlives the
    // call.
    unsafe { raise(capi::PyExc_IOError, message) };
}

/// `PyErr_SetString( PyExc_RuntimeError, ... )` (`bw_profile.cpp:294`).
fn raise_runtime_error(message: &str) {
    // SAFETY: as above.
    unsafe { raise(capi::PyExc_RuntimeError, message) };
}

/// The `raise` step on a global exception.
///
/// # Safety
///
/// The interpreter holds the GIL and `exception` is a live exception type.
unsafe fn raise(exception: *mut capi::PyObject, message: &str) {
    let encoded = CString::new(message.replace('\0', " ")).unwrap_or_default();
    // SAFETY: the exception global is live and the encoded message
    // outlives the call.
    unsafe { capi::PyErr_SetString(exception, encoded.as_ptr()) };
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// One pinned-clock frame at the test stamp rate (1 stamp = 1 ms),
    /// installed into the process global.
    struct Pinned(Arc<AtomicU64>);

    impl Pinned {
        fn install() -> Self {
            let cell = Arc::new(AtomicU64::new(0));
            let read = Arc::clone(&cell);
            install_test_profiler(ProfilerFrame::with_clock(
                1 << 20,
                8,
                move || read.load(Ordering::Relaxed),
                1_000_000.0,
            ));
            Pinned(cell)
        }

        fn advance(&self, by: u64) {
            self.0.fetch_add(by, Ordering::Relaxed);
        }
    }

    #[test]
    fn the_table_is_the_reference_s_eleven_rows() {
        let expected: &[(&str, bool, &str)] = &[
            (
                "__onThreadStart",
                true,
                "Thread start hook; begins profiling a thread",
            ),
            (
                "__onThreadEnd",
                false,
                "Thread end hook; finishes profiling a thread",
            ),
            ("enable", true, "Enable or disable performance profiling"),
            (
                "isEnabled",
                true,
                "Returns whether performance profiling currently running",
            ),
            ("tick", false, "Profiler Tick event"),
            ("startJsonDump", false, "Create a json dump file"),
            ("setJsonDumpCount", true, "Set the Json Dump Count "),
            ("setJsonDumpDir", true, "Set the Json Dump Dir "),
            ("getJsonDumpState", false, "Get the Json Dump State"),
            (
                "getJsonDumpStateValue",
                true,
                "Get the equivalent dump state enum value of the argument string",
            ),
            (
                "getJsonOutputFilePath",
                true,
                "Get the latest json output file path ",
            ),
        ];
        assert_eq!(methods().len(), expected.len());
        for (method, (name, varargs, doc)) in methods().iter().zip(expected) {
            assert_eq!(method.name, *name);
            assert_eq!(method.varargs, *varargs);
            assert_eq!(method.doc, *doc);
        }

        // The module identity and the flag values the table builds with.
        assert_eq!(MODULE_NAME.to_str().unwrap(), "_bw_profile");
        assert_eq!(MODULE_DOC.to_str().unwrap(), "BigWorld bw_profile module");
        assert_eq!(MAX_PROFILER_MEMORY_BYTES, 128 * 1024 * 1024);
        assert_eq!(capi::METH_VARARGS, 1);
        assert_eq!(capi::METH_NOARGS, 4);
    }

    #[test]
    fn the_dump_state_strings_map_through_the_wire_values() {
        assert_eq!(dump_state_value("JSON_DUMPING_INACTIVE"), Ok(0));
        assert_eq!(dump_state_value("JSON_DUMPING_ACTIVE"), Ok(1));
        assert_eq!(dump_state_value("JSON_DUMPING_COMPLETED"), Ok(2));
        assert_eq!(dump_state_value("JSON_DUMPING_FAILED"), Ok(3));
        // An unknown name is the exact RuntimeError sentence.
        assert_eq!(
            dump_state_value("JSON_DUMPING_NOPE"),
            Err("getJsonDumpStateValue: Invalid dump state value JSON_DUMPING_NOPE".to_string())
        );
        assert_eq!(
            dump_state_value(""),
            Err("getJsonDumpStateValue: Invalid dump state value ".to_string())
        );
    }

    #[test]
    fn the_write_probe_refuses_what_access_refuses() {
        let dir = std::env::temp_dir().join(format!("bw_profile_probe_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let text = dir.to_str().unwrap().to_string();

        // A directory the process can write to passes.
        assert_eq!(directory_writable(&text), Ok(()));

        // A path that cannot exist fails with the strerror text.
        let missing = dir.join("bw_profile_missing_subdir");
        let missing_text = missing.to_str().unwrap().to_string();
        let error = directory_writable(&missing_text).unwrap_err();
        assert!(error.reason.contains("No such file"), "{:?}", error);

        // The sentences wrap the reason exactly (`bw_profile.cpp:213-224`).
        let message = format!(
            "Unable to write to JSON dump directory: {missing_text}: {}.",
            error.reason
        );
        assert_eq!(error.message(&missing_text), message);
        assert_eq!(
            error.exception_message(&missing_text),
            format!("setJsonDumpDir: {message}")
        );

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_trace_actions_map_to_the_frame_events() {
        // CALL/RETURN: the frame name with the matching event.
        assert_eq!(
            trace_entry(capi::PyTrace_CALL, Some("foo"), None),
            Some(("foo".to_string(), EventType::Start))
        );
        assert_eq!(
            trace_entry(capi::PyTrace_RETURN, Some("foo"), None),
            Some(("foo".to_string(), EventType::End))
        );
        // A frame with no readable name reports "<unknown>" (:90).
        assert_eq!(
            trace_entry(capi::PyTrace_CALL, None, None),
            Some((UNKNOWN_NAME.to_string(), EventType::Start))
        );

        // The C actions carry the C function's name, only when the
        // PyCFunction gate passed (:99/:109).
        assert_eq!(
            trace_entry(capi::PyTrace_C_CALL, Some("frame"), Some("open")),
            Some(("open".to_string(), EventType::Start))
        );
        assert_eq!(trace_entry(capi::PyTrace_C_CALL, Some("frame"), None), None);
        assert_eq!(
            trace_entry(capi::PyTrace_C_RETURN, None, Some("open")),
            Some(("open".to_string(), EventType::End))
        );
        assert_eq!(
            trace_entry(capi::PyTrace_C_EXCEPTION, None, Some("open")),
            Some(("open".to_string(), EventType::End))
        );

        // LINE/OP/EXCEPTION and the 3.11+ additions record nothing.
        for action in [
            capi::PyTrace_LINE,
            capi::PyTrace_OP,
            capi::PyTrace_EXCEPTION,
            capi::PyTrace_PY_CALL,
            capi::PyTrace_RCALL,
            99,
        ] {
            assert_eq!(trace_entry(action, Some("frame"), Some("open")), None);
        }
    }

    #[test]
    fn the_faces_drive_the_frame_the_way_the_c_functions_do() {
        let _serial = global_slot();
        let pinned = Pinned::install();

        // isEnabled starts at 0: the constructor's MainThread add does not
        // enable.
        assert_eq!(face_is_enabled(), 0);
        assert_eq!(face_get_json_dump_state(), JsonDumpState::Inactive.as_int());

        // enable(true) only pends, but the watcher face reports up; tick
        // consumes the pending flag (BW-12-03-01's contract).
        face_enable(true);
        assert_eq!(face_is_enabled(), 1);
        face_tick();
        assert_eq!(face_is_enabled(), 1);

        // The dump request/count faces land in the frame.
        face_start_json_dump();
        assert!(profiler().watcher_get_dump_profile());
        face_set_json_dump_count(3);
        assert_eq!(profiler().json_dump_count(), 3);

        // The thread hooks: the port pins `OurThreadID()` to one id
        // (`profiler_frame.rs`'s `add_this_thread`), so a second start call
        // is the reference's "already added" re-registration rather than a
        // new slot — the warning is the observable.
        let warned_before = profiler().warnings.len();
        face_on_thread_start("ScriptThread");
        assert_eq!(profiler().get_thread_name(0), Some("MainThread"));
        assert_eq!(profiler().warnings.len(), warned_before + 1);
        assert!(
            profiler()
                .warnings
                .last()
                .unwrap()
                .contains("already added")
        );

        // The end hook removes our thread id, deferred while profiling is
        // active (`remove_thread`'s gate).
        face_on_thread_end();
        assert_eq!(profiler().get_thread_name(0), Some("MainThread"));

        // The dump-dir face stores what the probe accepts and refuses what
        // it does not, with the exact sentences.
        let dir = std::env::temp_dir();
        face_set_json_dump_dir(dir.to_str().unwrap()).unwrap();
        assert_eq!(profiler().json_dump_dir(), dir.to_str().unwrap());
        let missing = dir.join("bw_profile_missing_dir_probe");
        let error = face_set_json_dump_dir(missing.to_str().unwrap()).unwrap_err();
        assert_eq!(
            error.exception_message(missing.to_str().unwrap()),
            format!(
                "setJsonDumpDir: Unable to write to JSON dump directory: {}: {}.",
                missing.to_str().unwrap(),
                error.reason
            )
        );

        // enable(false) takes the watcher face down and resets the dump
        // state on both edges (profiler.cpp:2550/:2559).
        profiler().set_json_dump_state(JsonDumpState::Completed);
        face_enable(false);
        assert_eq!(face_is_enabled(), 0);
        assert_eq!(face_get_json_dump_state(), JsonDumpState::Inactive.as_int());

        // The removal was deferred, so the entry-processing task is what
        // empties the slot and renames it "Deleted"
        // (`internal_remove_thread`).
        profiler().do_remove_threads();
        assert_eq!(profiler().get_thread_name(0), None);

        // The output-path face reads what the dump wrote.
        profiler().set_json_output_file_path("/tmp/bw_profile_dump.json");
        assert_eq!(
            face_get_json_output_file_path(),
            "/tmp/bw_profile_dump.json"
        );

        // The pinned clock is live through the faces: a scripted frame
        // records against the same stamps the report reads.
        pinned.advance(10);
        profiler().add_entry("face", EventType::Start, 0, Category::PYTHON);
    }

    /// BW-12-03-02's acceptance: the import, the state round-trip, and the
    /// callback, compared against the reference. `PyInit__bw_profile`
    /// (`bw_profile.cpp:315-352`) sizes the pool, links the table, and raises
    /// the trace callback, so the scenario repeats those three steps in that
    /// order and then drives every face from Python.
    ///
    /// Ignored: `PyEval_SetProfile` is process-wide and cannot be unwound, so
    /// the parent runs the scenario out-of-process.
    #[test]
    #[ignore = "runs in its own process: see SCENARIOS"]
    fn scenario_the_module_imports_and_answers_the_state_faces() {
        // `Script::init` needs the engine's script tree on its path, the way
        // the interpreter scenarios' fixture stands in for it.
        let root = std::env::temp_dir().join(format!(
            "boundless-python-host-bw-profile-{}",
            std::process::id()
        ));
        std::fs::remove_dir_all(&root).ok();
        std::fs::create_dir_all(root.join("scripts/common")).expect("build the script tree");
        std::fs::write(
            root.join("scripts/common/bw_site.py"),
            "import sys\nsys.bw_site_imported = True\n",
        )
        .expect("write bw_site");
        let root_path = root.to_str().expect("a UTF-8 root").to_owned();
        let mut paths = crate::ImportPaths::default();
        paths.add_res_path(&[&root_path], crate::import_paths::COMMON_PATH);
        let interpreter = crate::interpreter::Interpreter::init(&crate::interpreter::InitConfig {
            import_paths: paths,
            ..Default::default()
        })
        .expect("Script::init");

        // `PyInit__bw_profile`: the pool, the trace callback, the table.
        assert!(init(), "PyInit sizes the pool and raises the callback");
        // SAFETY: the interpreter is up and this thread owns the GIL.
        let module = unsafe { capi::PyImport_AddModule(c"_bw_profile".as_ptr()) };
        assert!(!module.is_null());
        assert!(unsafe { install(module) }, "PyModule_AddFunctions");

        // `__probe`, the way the interpreter scenarios read it back.
        let show = |source: &str| -> String {
            assert!(
                interpreter.run_simple_string(source),
                "script must run: {source}"
            );
            // SAFETY: the interpreter is up and this thread owns the GIL.
            unsafe {
                let main = capi::PyImport_AddModule(c"__main__".as_ptr());
                let dict = capi::PyModule_GetDict(main);
                let value = capi::PyDict_GetItemString(dict, c"__probe".as_ptr());
                let text = capi::PyObject_Str(value);
                let rendered = capi::to_string(text);
                capi::dec_ref(text);
                rendered
            }
        };

        // The import, the thread hooks, the state faces and the dump faces,
        // every one answered from Python. The trace callback is raised before
        // `traced` is defined, so the definition and the call are profiled:
        // a callback that left an error behind would fail `run_simple_string`.
        let shown = show(
            "import _bw_profile as m\n\
             m.__onThreadStart('MainThread')\n\
             m.enable(1)\n\
             def traced():\n\
             \treturn 4\n\
             traced()\n\
             m.tick()\n\
             m.setJsonDumpCount(3)\n\
             m.startJsonDump()\n\
             m.setJsonDumpDir('/tmp')\n\
             __probe = (m.isEnabled(), m.getJsonDumpState(),\n\
             \tm.getJsonDumpStateValue('JSON_DUMPING_COMPLETED'),\n\
             \tm.getJsonOutputFilePath(), m.__onThreadEnd())",
        );
        assert_eq!(shown, "(1, 0, 2, '', None)");
        assert_eq!(profiler().get_thread_name(0), Some("MainThread"));

        // `__onThreadEnd` defers its removal: the task the frame processing
        // runs is what empties the slot (`profiler.cpp:1679-1708`).
        profiler().do_remove_threads();
        assert_eq!(profiler().get_thread_name(0), None);

        // The refusal sentences are the reference's verbatim, both arms.
        let refused = show(
            "import _bw_profile as m\n\
             try:\n\
             \tm.setJsonDumpDir('/no/such/dump_dir_xyz')\n\
             \t__io = 'none'\n\
             except OSError as e:\n\
             \t__io = str(e)\n\
             try:\n\
             \tm.getJsonDumpStateValue('NOPE')\n\
             \t__rt = 'none'\n\
             except RuntimeError as e:\n\
             \t__rt = str(e)\n\
             try:\n\
             \tm.enable('not an int')\n\
             \t__ty = 'none'\n\
             except TypeError:\n\
             \t__ty = 'TypeError'\n\
             __probe = (__io, __rt, __ty)",
        );
        assert_eq!(
            refused,
            "('setJsonDumpDir: Unable to write to JSON dump directory: \
             /no/such/dump_dir_xyz: No such file or directory.', \
             'getJsonDumpStateValue: Invalid dump state value NOPE', 'TypeError')"
        );

        interpreter.fini();
    }
}
