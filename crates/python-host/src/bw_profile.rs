//! BW-12-03-02: the `_bw_profile` module
//! (`server/tools/bw_profile/bw_profile.cpp`).
//!
//! The reference ships the profiler's start/stop face to Python as a
//! built-in extension: `PyInit__bw_profile` initialises the profiler with a
//! fixed 128 MB budget and installs [`profile_func`] as the thread's trace
//! callback, and the eleven-entry method table turns Python calls into
//! `g_profiler` calls — thread hooks (`__onThreadStart`/`__onThreadEnd`),
//! the enable/isEnabled watcher face, the tick, and the JSON dump controls
//! whose writer is BW-12-03-03.
//!
//! # Port shape
//!
//! - The module object comes from `PyModule_Create2` over a static
//!   [`PyModuleDef`], the reference's own call (`bw_profile.cpp:336`): the
//!   import machinery reads `md_def` back off a single-phase inittab
//!   module, so `PyModule_New` alone does not produce a valid extension
//!   module. The def must sit in writable memory — see the
//!   [`MODULE_DEF`] comment for the RELRO trap `python-module` documented.
//! - Reaching the importable name goes through
//!   `PyImport_AppendInittab` ([`register_for_import`]) instead of a
//!   dynamically loaded `_bw_profile.so`: the embedded host links the
//!   module in, which is the reference's own mechanism for linked-in
//!   modules (`tools/worldeditor/scripting/world_editor_script.cpp:2165`).
//!   `AppendInittab` must run before `Py_Initialize`.
//! - `g_profiler` is [`profiler::global`] (BW-12-03-01). The reference
//!   initialises with `g_profiler.init( MAX_PROFILER_MEMORY_BYTES )`
//!   (`bw_profile.cpp:332-334`), i.e. the default thread cap; init also
//!   registers the calling thread as "MainThread" (`profiler.cpp:1049`),
//!   so the importing thread's traced calls attribute to slot 0 from the
//!   first entry.
//! - The two `ERROR_MSG` log lines (`bw_profile.cpp:219-220,287-288`) go to
//!   the debug-message face this port has not built yet (BW-18); the
//!   exception texts are the observable half and are carried verbatim.
//!   `bw_profile.cpp:219` labels the `setJsonDumpDir` failure with a
//!   copy-pasted `getJsonDumpStateValue` prefix — a reference bug on the
//!   log-only path, noted but not reproduced here.

// The trace-action constants keep the C API's `PyTrace_*` spelling
// verbatim; the lint's screaming-case rule yields to the reference names.
#![allow(non_upper_case_globals)]

use std::ffi::{CStr, CString, c_char, c_int};
use std::os::raw::c_long;

use profiler::{CATEGORY_PYTHON, EventType, JsonDumpState, MAXIMUM_NUMBER_OF_THREADS, global};

use crate::capi::{self, PyMethodDef, PyObject};

/// `PyTrace_CALL` (`Include/cpython/ceval.h`, `enum _pytrace_t`): a Python
/// frame was entered.
const PyTrace_CALL: c_int = 0;
/// `PyTrace_RETURN`: a Python frame is about to return.
const PyTrace_RETURN: c_int = 3;
/// `PyTrace_C_CALL`: a C function (`PyCFunction`) is about to be called.
const PyTrace_C_CALL: c_int = 4;
/// `PyTrace_C_RETURN`: a C function returned.
const PyTrace_C_RETURN: c_int = 5;
/// `PyTrace_C_EXCEPTION`: a C function raised.
const PyTrace_C_EXCEPTION: c_int = 6;

/// `METH_NOARGS` (`Include/methodobject.h:96`): the handler receives a null
/// second argument.
const METH_NOARGS: c_int = 0x0004;

/// `Py_tracefunc` (`Include/eval.h`): the signature `PyEval_SetProfile`
/// installs.
type PyTraceFunc =
    unsafe extern "C" fn(*mut PyObject, *mut PyFrameObject, c_int, *mut PyObject) -> c_int;

/// `PyFrameObject`: opaque. Only [`PyFrame_GetCode`] crosses into it, the
/// accessor the reference's own 3.13 migration note calls for
/// (`bw_profile.cpp:79-80`).
#[repr(C)]
pub struct PyFrameObject {
    _private: (),
}

/// `PyCFunctionObject` (`Include/methodobject.h:63`) trimmed to the one
/// field the tracer reads: the descriptor pointer sits right after the
/// two-word object header (the layout test below pins the offset).
#[repr(C)]
struct PyCFunctionObject {
    #[allow(dead_code)]
    ob_refcnt: isize,
    #[allow(dead_code)]
    ob_type: *mut capi::PyTypeObject,
    /// `m_ml`, the `PyMethodDef` whose `ml_name` the reference reads
    /// (`bw_profile.cpp:102`).
    m_ml: *const PyMethodDef,
}

/// `PyModuleDef_Base` (`Include/moduleobject.h:39`), the 40-byte header
/// every module definition carries. The layouts are the ones
/// `python-module` compiled a probe against (3.14); a drift segfaults the
/// import, so the offsets are pinned in the tests.
#[repr(C)]
struct PyModuleDefBase {
    /// `ob_refcnt`, initialised to the static immortal value.
    ob_refcnt: isize,
    /// `ob_type`.
    ob_type: *mut PyObject,
    /// `m_init`, the single-phase re-initialiser (null here).
    m_init: *const std::os::raw::c_void,
    /// `m_index`, the module's index in the interpreter's module cache.
    m_index: isize,
    /// `m_copy`, the cached `__dict__` copy (null until first load).
    m_copy: *mut PyObject,
}

/// `PyModuleDef` (`Include/moduleobject.h:107`), the 104-byte module
/// definition — the reference's `s_bwProfileModule`
/// (`bw_profile.cpp:317-324`).
#[repr(C)]
struct PyModuleDef {
    /// `m_base`, the [`PyModuleDefBase`] header.
    m_base: PyModuleDefBase,
    /// `m_name`, the module name.
    m_name: *const c_char,
    /// `m_doc`, the module docstring.
    m_doc: *const c_char,
    /// `m_size`, `-1` marks a single-phase (legacy) module.
    m_size: isize,
    /// `m_methods`, the [`PyMethodDef`] table.
    m_methods: *mut PyMethodDef,
    /// `m_slots`, the multi-phase init slots (null for single-phase).
    m_slots: *mut std::os::raw::c_void,
    /// `m_traverse`, the GC traverse function.
    m_traverse: *const std::os::raw::c_void,
    /// `m_clear`, the clear function.
    m_clear: *const std::os::raw::c_void,
    /// `m_free`, the dealloc function.
    m_free: *const std::os::raw::c_void,
}

/// `PyModuleDef_HEAD_INIT`, hand-set the way `object.h`'s
/// `PyObject_HEAD_INIT(NULL)` expands in a GIL-enabled build: `ob_refcnt`
/// is the static immortal value, `ob_type` null.
const HEAD_INIT: PyModuleDefBase = PyModuleDefBase {
    ob_refcnt: 7 << 28,
    ob_type: std::ptr::null_mut(),
    m_init: std::ptr::null(),
    m_index: 0,
    m_copy: std::ptr::null_mut(),
};

/// The method-table pointer for the module definition.
const METHODS_PTR: *mut PyMethodDef = METHOD_TABLE.0.as_ptr() as *mut PyMethodDef;

// SAFETY: these definitions are `static` descriptions the module only ever
// reads, and the pointers they carry get their relocations at load time.
unsafe impl Sync for PyModuleDefBase {}
unsafe impl Sync for PyModuleDef {}

/// The module definition: a `static`, not a `const`, so the pointer handed
/// to `PyModule_Create2` is a stable address — CPython keeps `md_def` on
/// the module and re-reads it on re-import.
///
/// `PyModule_Create2` writes into the def it is given (`ob_refcnt`,
/// `ob_type`, `m_index`), so the definition must live in writable memory.
/// The linker's default for a `static` with pointer relocations is
/// `.data.rel.ro`, which full RELRO (`-z now`) makes read-only after
/// relocation — that write then segfaults on import. The `.data` section is
/// outside the RELRO segment, so this keeps the def writable while the
/// pointers it carries still get their load-time relocations applied.
#[unsafe(link_section = ".data")]
static MODULE_DEF: PyModuleDef = PyModuleDef {
    m_base: HEAD_INIT,
    m_name: MODULE_NAME.as_ptr(),
    m_doc: MODULE_DOC.as_ptr(),
    m_size: -1,
    m_methods: METHODS_PTR,
    m_slots: std::ptr::null_mut(),
    m_traverse: std::ptr::null(),
    m_clear: std::ptr::null(),
    m_free: std::ptr::null(),
};

/// `PYTHON_API_VERSION` (`Include/modsupport.h:59` for 3.13/3.14): the ABI
/// version handed to `PyModule_Create2`; `check_api_version` rejects a
/// mismatch with a `SystemError`.
const PYTHON_API_VERSION: c_int = 1013;

unsafe extern "C" {
    /// `PyModule_Create2` (`Include/modsupport.h:111`): allocate the module
    /// object from a static `PyModuleDef` — the call the reference's
    /// `PyModule_Create` macro expands to. A single-phase inittab module
    /// must carry the definition (the import machinery reads `md_def` back
    /// off the module), so `PyModule_New` alone is not enough here.
    fn PyModule_Create2(def: *mut PyModuleDef, apiver: c_int) -> *mut PyObject;

    /// `PyEval_SetProfile` (`Include/ceval.h`): install the thread's trace
    /// callback. A null function clears it, as `__onThreadEnd` does
    /// (`bw_profile.cpp:142`).
    fn PyEval_SetProfile(func: Option<PyTraceFunc>, arg: *mut PyObject);

    /// `PyFrame_GetCode` (`Include/cpython/frameobject.h`): the frame's
    /// code object, borrowed.
    fn PyFrame_GetCode(frame: *mut PyFrameObject) -> *mut PyObject;

    /// `PyImport_AppendInittab` (`Include/import.h`): register a built-in
    /// module's init function; must precede `Py_Initialize`.
    fn PyImport_AppendInittab(
        name: *const c_char,
        init: Option<unsafe extern "C" fn() -> *mut PyObject>,
    ) -> c_int;

    /// `PyCFunction_Type` (`Include/methodobject.h`): the
    /// builtin-function type the tracer's C-event argument is checked
    /// against.
    pub static mut PyCFunction_Type: capi::PyTypeObject;

    /// `PyExc_RuntimeError` (`Include/errno.h`-via-`pyerrors.h`): raised by
    /// `getJsonDumpStateValue` for an unknown name.
    pub static mut PyExc_RuntimeError: *mut PyObject;
}

/// The module name `PyInit__bw_profile` registers (`bw_profile.cpp:319`).
const MODULE_NAME: &CStr = c"_bw_profile";

/// The `m_doc` of the module definition (`bw_profile.cpp:320`).
const MODULE_DOC: &CStr = c"BigWorld bw_profile module";

/// `Py_RETURN_NONE` (`Include/ceval.h`-adjacent macro): hand back an owned
/// None.
///
/// # Safety
///
/// The caller owns the GIL.
unsafe fn py_return_none() -> *mut PyObject {
    let none = capi::py_none();
    // SAFETY: the returned value carries a new reference, as every handler
    // return does.
    unsafe { capi::inc_ref(none) };
    none
}

/// The C view of a message the reference would hand to `PyErr_SetString` /
/// `PyUnicode_FromString` as `BW::string::c_str()`: a NUL inside the text
/// truncates the C view at that byte, and the port truncates the same way
/// explicitly rather than panic on `CString::new`.
fn c_message(text: &str) -> CString {
    let truncated: Vec<u8> = text.bytes().take_while(|&byte| byte != 0).collect();
    CString::new(truncated).expect("no interior NUL remains")
}

/// The `errMessage` build (`bw_profile.cpp:213-217`): the fixed lead, the
/// directory, the libc error text and the trailing period.
fn dump_dir_error_message(dir: &str, strerror: &str) -> String {
    format!("Unable to write to JSON dump directory: {dir}: {strerror}.")
}

/// The exception prefix (`bw_profile.cpp:223-224`).
fn dump_dir_exception_message(err_message: &str) -> String {
    format!("setJsonDumpDir: {err_message}")
}

/// `strerror( errno )` at the failure point (`bw_profile.cpp:216`): the
/// text comes from libc and is locale-dependent, so tests match around it.
fn strerror_text() -> String {
    let errno = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
    // SAFETY: `strerror` returns a pointer into libc's static message
    // table, NUL-terminated for every known errno.
    unsafe {
        CStr::from_ptr(libc::strerror(errno))
            .to_string_lossy()
            .into_owned()
    }
}

/// The strcmp chain (`bw_profile.cpp:269-284`): the names resolve to the
/// profiler's own enum values, never a hardcoded copy, so a change to the
/// enum in `profiler.hpp` flows through (`bw_profile.cpp:246-258`).
fn dump_state_value(str_state: &str) -> Option<i32> {
    match str_state {
        "JSON_DUMPING_INACTIVE" => Some(JsonDumpState::Inactive as i32),
        "JSON_DUMPING_ACTIVE" => Some(JsonDumpState::Active as i32),
        "JSON_DUMPING_COMPLETED" => Some(JsonDumpState::Completed as i32),
        "JSON_DUMPING_FAILED" => Some(JsonDumpState::Failed as i32),
        _ => None,
    }
}

/// The `PyCFunction_Check` gate plus the `m_ml->ml_name` read
/// (`bw_profile.cpp:99-115`): the tracer's C-event argument is the callable
/// object, and builtin functions are exactly `PyCFunction_Type` — the
/// subtype walk the macro performs cannot add names this read would miss,
/// so the port compares the type pointer directly. Anything else (a null
/// argument, a non-function callable) yields nothing, matching the
/// reference's guarded skip. On the pinned interpreter the guard's reject
/// path is a reference-era face kept verbatim: 3.13 binds method and
/// classmethod descriptors to a `PyCFunction` before the event is emitted,
/// so traced C callables always pass the check.
///
/// # Safety
///
/// The interpreter owns the GIL and `arg` is the live trace argument.
unsafe fn traced_c_function_name(arg: *mut PyObject) -> Option<String> {
    if arg.is_null()
        || !std::ptr::eq(
            // SAFETY: the argument is a live object or null.
            unsafe { capi::Py_TYPE(arg) },
            std::ptr::addr_of_mut!(PyCFunction_Type),
        )
    {
        return None;
    }
    let function = arg as *const PyCFunctionObject;
    // SAFETY: the type check above guarantees the object layout.
    let name = unsafe { (*(*function).m_ml).ml_name };
    Some(
        // SAFETY: `ml_name` is a static NUL-terminated method name.
        unsafe { CStr::from_ptr(name) }
            .to_string_lossy()
            .into_owned(),
    )
}

/// The `profileFunc` trace callback (`bw_profile.cpp:76-120`): Python
/// frames contribute their `co_name` (via the object protocol the
/// reference's migration note prescribes), C calls contribute the method
/// descriptor's name, and everything else is ignored. Always returns 0.
///
/// # Safety
///
/// Called by CPython with the GIL held; `frame` is live for CALL/RETURN.
unsafe extern "C" fn profile_func(
    _object: *mut PyObject,
    frame: *mut PyFrameObject,
    action: c_int,
    arg: *mut PyObject,
) -> c_int {
    match action {
        PyTrace_CALL | PyTrace_RETURN => {
            // SAFETY: the frame is live for both events.
            let code = unsafe { PyFrame_GetCode(frame) };
            // SAFETY: the code object is live; a failed lookup leaves the
            // AttributeError set, exactly like the reference's unchecked
            // fall-through (`bw_profile.cpp:87-90`).
            let name_object = unsafe { capi::PyObject_GetAttrString(code, c"co_name".as_ptr()) };
            let name = if name_object.is_null() {
                "<unknown>".to_owned()
            } else {
                // SAFETY: `co_name` is a str.
                unsafe { capi::to_string(name_object) }
            };
            let event = if action == PyTrace_CALL {
                EventType::Start
            } else {
                EventType::End
            };
            // g_profiler.addEntry( name, ..., 0, CATEGORY_PYTHON ): the
            // entry copies the name into its fixed buffer before the
            // decref (`bw_profile.cpp:91-95`).
            profiler::global().add_entry(&name, event, 0, CATEGORY_PYTHON);
            // SAFETY: mirrors the reference's Py_XDECREF (`bw_profile.cpp:95`).
            unsafe { capi::dec_ref(name_object) };
        }
        PyTrace_C_CALL => {
            if let Some(name) =
                // SAFETY: the trace argument is live for C events.
                unsafe { traced_c_function_name(arg) }
            {
                profiler::global().add_entry(&name, EventType::Start, 0, CATEGORY_PYTHON);
            }
        }
        PyTrace_C_RETURN | PyTrace_C_EXCEPTION => {
            if let Some(name) =
                // SAFETY: the trace argument is live for C events.
                unsafe { traced_c_function_name(arg) }
            {
                profiler::global().add_entry(&name, EventType::End, 0, CATEGORY_PYTHON);
            }
        }
        _ => {
            // PyTrace_LINE and friends are not profiler faces
            // (`bw_profile.cpp:81-117` switch falls through). Unreachable
            // in practice: profile callbacks receive no Python-frame
            // EXCEPTION or LINE events on the pinned interpreter.
        }
    }
    0
}

/// `__onThreadStart` (`bw_profile.cpp:123-136`): register the calling
/// thread under `name`, then install the trace function.
///
/// # Safety
///
/// Called by CPython with the GIL held.
unsafe extern "C" fn method_on_thread_start(
    _self: *mut PyObject,
    args: *mut PyObject,
) -> *mut PyObject {
    let mut thread_name: *const c_char = std::ptr::null_mut();
    // SAFETY: the interpreter owns the GIL and `args` is the live tuple.
    unsafe {
        if capi::PyArg_ParseTuple(args, c"s".as_ptr(), &mut thread_name) == 0 {
            return std::ptr::null_mut();
        }
        let name = CStr::from_ptr(thread_name).to_string_lossy();
        global().add_this_thread(&name);
        PyEval_SetProfile(Some(profile_func), std::ptr::null_mut());
        py_return_none()
    }
}

/// `__onThreadEnd` (`bw_profile.cpp:139-145`): drop the calling thread and
/// clear the trace function.
///
/// # Safety
///
/// Called by CPython with the GIL held.
unsafe extern "C" fn method_on_thread_end(
    _self: *mut PyObject,
    _args: *mut PyObject,
) -> *mut PyObject {
    // SAFETY: the interpreter owns the GIL.
    unsafe {
        global().remove_thread(profiler::this_thread_id());
        PyEval_SetProfile(None, std::ptr::null_mut());
        py_return_none()
    }
}

/// `enable` (`bw_profile.cpp:148-160`): any nonzero argument enables.
///
/// # Safety
///
/// Called by CPython with the GIL held.
unsafe extern "C" fn method_enable(_self: *mut PyObject, args: *mut PyObject) -> *mut PyObject {
    let mut flag: c_int = 0;
    // SAFETY: the interpreter owns the GIL and `args` is the live tuple.
    unsafe {
        if capi::PyArg_ParseTuple(args, c"i".as_ptr(), &mut flag) == 0 {
            return std::ptr::null_mut();
        }
        // g_profiler.enable( (i == 0) ? false : true ) (bw_profile.cpp:157).
        global().enable(flag != 0);
        py_return_none()
    }
}

/// `isEnabled` (`bw_profile.cpp:163-168`): the watcher face as 1/0. The
/// reference registers the method `METH_VARARGS` and ignores the argument
/// tuple; the port keeps both facts.
///
/// # Safety
///
/// Called by CPython with the GIL held.
unsafe extern "C" fn method_is_enabled(
    _self: *mut PyObject,
    _args: *mut PyObject,
) -> *mut PyObject {
    // SAFETY: the interpreter owns the GIL.
    unsafe { capi::PyLong_FromLong(global().watcher_enabled() as c_long) }
}

/// `tick` (`bw_profile.cpp:171-176`).
///
/// # Safety
///
/// Called by CPython with the GIL held.
unsafe extern "C" fn method_tick(_self: *mut PyObject, _args: *mut PyObject) -> *mut PyObject {
    // SAFETY: the interpreter owns the GIL.
    unsafe {
        global().tick();
        py_return_none()
    }
}

/// `startJsonDump` (`bw_profile.cpp:179-184`): arms the dump watcher; the
/// dump itself is written on the tick, by BW-12-03-03's writer.
///
/// # Safety
///
/// Called by CPython with the GIL held.
unsafe extern "C" fn method_start_json_dump(
    _self: *mut PyObject,
    _args: *mut PyObject,
) -> *mut PyObject {
    // SAFETY: the interpreter owns the GIL.
    unsafe {
        global().watcher_set_dump_profile(true);
        py_return_none()
    }
}

/// `setJsonDumpCount` (`bw_profile.cpp:187-199`).
///
/// # Safety
///
/// Called by CPython with the GIL held.
unsafe extern "C" fn method_set_json_dump_count(
    _self: *mut PyObject,
    args: *mut PyObject,
) -> *mut PyObject {
    let mut count: c_int = 0;
    // SAFETY: the interpreter owns the GIL and `args` is the live tuple.
    unsafe {
        if capi::PyArg_ParseTuple(args, c"i".as_ptr(), &mut count) == 0 {
            return std::ptr::null_mut();
        }
        global().set_json_dump_count(count);
        py_return_none()
    }
}

/// `setJsonDumpDir` (`bw_profile.cpp:202-234`): the writability check runs
/// first; a refusal raises `IOError` with the constructed message.
///
/// # Safety
///
/// Called by CPython with the GIL held.
unsafe extern "C" fn method_set_json_dump_dir(
    _self: *mut PyObject,
    args: *mut PyObject,
) -> *mut PyObject {
    let mut dir: *const c_char = std::ptr::null_mut();
    // SAFETY: the interpreter owns the GIL and `args` is the live tuple.
    unsafe {
        if capi::PyArg_ParseTuple(args, c"s".as_ptr(), &mut dir) == 0 {
            return std::ptr::null_mut();
        }
        // BW_ACCESS( dir, BW_W_OK ) < 0 (bw_profile.cpp:211).
        if libc::access(dir, libc::W_OK) < 0 {
            let dir_text = CStr::from_ptr(dir).to_string_lossy();
            let strerror = strerror_text();
            let err_message = dump_dir_error_message(&dir_text, &strerror);
            let exception = dump_dir_exception_message(&err_message);
            let encoded = c_message(&exception);
            capi::PyErr_SetString(capi::PyExc_IOError, encoded.as_ptr());
            return std::ptr::null_mut();
        }
        let dir_text = CStr::from_ptr(dir).to_string_lossy();
        // g_profiler.setJsonDumpDir( BW::string( dir ) )
        // (bw_profile.cpp:231).
        global().set_json_dump_dir(&dir_text);
        py_return_none()
    }
}

/// `getJsonDumpState` (`bw_profile.cpp:237-243`).
///
/// # Safety
///
/// Called by CPython with the GIL held.
unsafe extern "C" fn method_get_json_dump_state(
    _self: *mut PyObject,
    _args: *mut PyObject,
) -> *mut PyObject {
    // SAFETY: the interpreter owns the GIL.
    unsafe { capi::PyLong_FromLong(global().get_json_dump_state() as i32 as c_long) }
}

/// `getJsonDumpStateValue` (`bw_profile.cpp:259-300`): the four known names
/// map to the profiler's enum; anything else raises `RuntimeError`.
///
/// # Safety
///
/// Called by CPython with the GIL held.
unsafe extern "C" fn method_get_json_dump_state_value(
    _self: *mut PyObject,
    args: *mut PyObject,
) -> *mut PyObject {
    let mut str_state: *const c_char = std::ptr::null_mut();
    // SAFETY: the interpreter owns the GIL and `args` is the live tuple.
    unsafe {
        if capi::PyArg_ParseTuple(args, c"s".as_ptr(), &mut str_state) == 0 {
            return std::ptr::null_mut();
        }
        let state = CStr::from_ptr(str_state).to_string_lossy();
        match dump_state_value(&state) {
            Some(value) => capi::PyLong_FromLong(value as c_long),
            None => {
                // The ERROR_MSG half of this branch
                // (`bw_profile.cpp:287-288`) belongs to the log face this
                // port has not built yet (BW-18).
                let message = format!("getJsonDumpStateValue: Invalid dump state value {state}");
                let encoded = c_message(&message);
                capi::PyErr_SetString(PyExc_RuntimeError, encoded.as_ptr());
                std::ptr::null_mut()
            }
        }
    }
}

/// `getJsonOutputFilePath` (`bw_profile.cpp:303-309`).
///
/// # Safety
///
/// Called by CPython with the GIL held.
unsafe extern "C" fn method_get_json_output_file_path(
    _self: *mut PyObject,
    _args: *mut PyObject,
) -> *mut PyObject {
    let path = global().get_json_output_file_path();
    let encoded = c_message(&path);
    // SAFETY: the interpreter owns the GIL and `encoded` outlives the call.
    unsafe { capi::PyUnicode_FromString(encoded.as_ptr()) }
}

/// A method table's shared handle: `PyMethodDef` carries raw pointers, so
/// the slice alone is not `Sync`, but this table is an immutable constant
/// that `PyModule_AddFunctions` only reads.
struct MethodTable(&'static [PyMethodDef]);
unsafe impl Sync for MethodTable {}

/// The reference's method table (`bw_profile.cpp:47-73`): eleven entries,
/// the docstrings verbatim (including the trailing spaces on
/// `setJsonDumpCount`/`setJsonDumpDir`/`getJsonOutputFilePath`), the flags
/// as registered, terminated by the null row.
static METHOD_TABLE: MethodTable = MethodTable(&[
    PyMethodDef {
        ml_name: c"__onThreadStart".as_ptr(),
        ml_meth: Some(method_on_thread_start),
        ml_flags: capi::METH_VARARGS,
        ml_doc: c"Thread start hook; begins profiling a thread".as_ptr(),
    },
    PyMethodDef {
        ml_name: c"__onThreadEnd".as_ptr(),
        ml_meth: Some(method_on_thread_end),
        ml_flags: METH_NOARGS,
        ml_doc: c"Thread end hook; finishes profiling a thread".as_ptr(),
    },
    PyMethodDef {
        ml_name: c"enable".as_ptr(),
        ml_meth: Some(method_enable),
        ml_flags: capi::METH_VARARGS,
        ml_doc: c"Enable or disable performance profiling".as_ptr(),
    },
    PyMethodDef {
        ml_name: c"isEnabled".as_ptr(),
        ml_meth: Some(method_is_enabled),
        ml_flags: capi::METH_VARARGS,
        ml_doc: c"Returns whether performance profiling currently running".as_ptr(),
    },
    PyMethodDef {
        ml_name: c"tick".as_ptr(),
        ml_meth: Some(method_tick),
        ml_flags: METH_NOARGS,
        ml_doc: c"Profiler Tick event".as_ptr(),
    },
    PyMethodDef {
        ml_name: c"startJsonDump".as_ptr(),
        ml_meth: Some(method_start_json_dump),
        ml_flags: METH_NOARGS,
        ml_doc: c"Create a json dump file".as_ptr(),
    },
    PyMethodDef {
        ml_name: c"setJsonDumpCount".as_ptr(),
        ml_meth: Some(method_set_json_dump_count),
        ml_flags: capi::METH_VARARGS,
        ml_doc: c"Set the Json Dump Count ".as_ptr(),
    },
    PyMethodDef {
        ml_name: c"setJsonDumpDir".as_ptr(),
        ml_meth: Some(method_set_json_dump_dir),
        ml_flags: capi::METH_VARARGS,
        ml_doc: c"Set the Json Dump Dir ".as_ptr(),
    },
    PyMethodDef {
        ml_name: c"getJsonDumpState".as_ptr(),
        ml_meth: Some(method_get_json_dump_state),
        ml_flags: METH_NOARGS,
        ml_doc: c"Get the Json Dump State".as_ptr(),
    },
    PyMethodDef {
        ml_name: c"getJsonDumpStateValue".as_ptr(),
        ml_meth: Some(method_get_json_dump_state_value),
        ml_flags: capi::METH_VARARGS,
        ml_doc: c"Get the equivalent dump state enum value of the argument string".as_ptr(),
    },
    PyMethodDef {
        ml_name: c"getJsonOutputFilePath".as_ptr(),
        ml_meth: Some(method_get_json_output_file_path),
        ml_flags: capi::METH_VARARGS,
        ml_doc: c"Get the latest json output file path ".as_ptr(),
    },
    PyMethodDef::END,
]);

/// `PyInit__bw_profile` (`bw_profile.cpp:326-346`): initialise the profiler
/// with the module's fixed budget, build the module object, install the
/// trace function and return the module.
///
/// The reference opens with `BW_SYSTEMSTAGE_MAIN()` to mark the importing
/// thread main; the port has no system-stage registry yet, so the marker
/// has no counterpart. The 128 MB budget is as hardcoded here as the
/// reference's `TODO: make this configurable` leaves it
/// (`bw_profile.cpp:332-334`).
///
/// # Safety
///
/// Called by the import machinery with the interpreter up (or handed to
/// [`register_for_import`] before it starts).
pub unsafe extern "C" fn py_init_bw_profile() -> *mut PyObject {
    /// The reference's `MAX_PROFILER_MEMORY_BYTES` (`bw_profile.cpp:333`).
    const MAX_PROFILER_MEMORY_BYTES: i32 = 128 * 1024 * 1024;
    // SAFETY: the caller owns the GIL. The reference discards init's
    // result (`bw_profile.cpp:334`), and the call carries the default
    // thread cap.
    let _ = global().init(MAX_PROFILER_MEMORY_BYTES, MAXIMUM_NUMBER_OF_THREADS as i32);
    // PyObject *pModuleObj = PyModule_Create( &s_bwProfileModule )
    // (bw_profile.cpp:336): the definition carries the name, doc and
    // method table.
    // SAFETY: the interpreter owns the GIL and the def is a writable
    // static (see [`MODULE_DEF`]).
    let module = unsafe {
        // The def static is written through by `PyModule_Create2` (see its
        // comment); the cast is the documented escape from the `static`'s
        // immutability, the way `python-module` hands its def over.
        PyModule_Create2(
            &MODULE_DEF as *const PyModuleDef as *mut PyModuleDef,
            PYTHON_API_VERSION,
        )
    };
    if module.is_null() {
        return std::ptr::null_mut();
    }
    // PyEval_SetProfile( &profileFunc, NULL ) (bw_profile.cpp:343).
    // SAFETY: the interpreter owns the GIL.
    unsafe { PyEval_SetProfile(Some(profile_func), std::ptr::null_mut()) };
    module
}

/// Registers the module for `import _bw_profile` in an interpreter this
/// process has not started yet: `PyImport_AppendInittab` must run before
/// `Py_Initialize`. The reference loads `_bw_profile.so` dynamically; the
/// embedded host reaches the same importable name through the inittab, the
/// reference's own mechanism for linked-in modules.
pub fn register_for_import() -> bool {
    // SAFETY: no interpreter is up yet (that is the inittab contract) and
    // the init function is a static handler.
    unsafe { PyImport_AppendInittab(MODULE_NAME.as_ptr(), Some(py_init_bw_profile)) == 0 }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interpreter::{InitConfig, Interpreter};
    use std::process::Command;

    /// Every scenario, each in its own process: the interpreter refuses a
    /// second init in one process, the same rule `billing`'s harness
    /// follows.
    const SCENARIOS: &[&str] = &[
        "bw_profile::tests::scenario_the_module_imports_with_the_reference_method_table",
        "bw_profile::tests::scenario_enable_and_is_enabled_track_the_watcher_face",
        "bw_profile::tests::scenario_argument_shapes_refuse_the_wrong_types",
        "bw_profile::tests::scenario_get_json_dump_state_value_maps_the_four_names",
        "bw_profile::tests::scenario_set_json_dump_dir_checks_write_access",
        "bw_profile::tests::scenario_the_json_faces_report_the_armed_state",
        "bw_profile::tests::scenario_python_calls_become_category_python_entries",
        "bw_profile::tests::scenario_on_thread_end_unregisters_and_untraces",
    ];

    /// Runs `scenario` in a fresh process of this test binary and requires
    /// it to exit normally.
    fn run_scenario(scenario: &str) {
        let harness = std::env::current_exe().expect("the test binary path");
        let output = Command::new(&harness)
            .args(["--exact", scenario, "--ignored", "--test-threads=1"])
            .output()
            .expect("spawn the scenario child");
        assert!(
            output.status.success(),
            "scenario {scenario} did not exit normally: {}\n--- stdout ---\n{}\n--- stderr ---\n{}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
    }

    /// Registers the module, runs `Interpreter::init` around one body,
    /// then `Script::fini`.
    fn interpreter_scenario(body: impl FnOnce()) {
        assert!(register_for_import(), "AppendInittab");
        let interpreter = Interpreter::init(&scenario_config()).expect("Script::init");
        body();
        interpreter.fini();
    }

    /// `Interpreter::init`'s config with a throw-away resource root in the
    /// path list, so `Script::init`'s own `import bw_site` finds a module
    /// (`script.cpp:617`).
    fn scenario_config() -> InitConfig {
        let root = temp_resource_root();
        let root_path = root.to_str().expect("a UTF-8 root").to_owned();
        let mut config = InitConfig::default();
        config
            .import_paths
            .add_res_path(&[&root_path], crate::import_paths::COMMON_PATH);
        config
    }

    /// A throw-away resource root laid out the way the engine ships its
    /// scripts.
    fn temp_resource_root() -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!(
            "boundless-python-host-bw-profile-{}",
            std::process::id()
        ));
        std::fs::remove_dir_all(&root).ok();
        std::fs::create_dir_all(root.join("scripts/common/Lib")).expect("build the script tree");
        std::fs::write(
            root.join("scripts/common/bw_site.py"),
            "import sys\nsys.bw_site_imported = True\n",
        )
        .expect("write bw_site");
        root
    }

    /// Runs `source` and reports whether it ran.
    fn run_simple(source: &str) -> bool {
        // SAFETY: only called while the interpreter is up.
        unsafe {
            let Ok(encoded) = CString::new(source) else {
                return false;
            };
            if capi::PyRun_SimpleString(encoded.as_ptr()) != 0 {
                capi::clear_error();
                return false;
            }
            true
        }
    }

    /// The `__probe` global rendered as a string; scenario functions
    /// declare `global __probe` before assigning it.
    fn read_probe() -> String {
        read_global(b"__probe\0")
    }

    #[test]
    #[ignore = "runs in its own process: see SCENARIOS"]
    fn scenario_the_module_imports_with_the_reference_method_table() {
        interpreter_scenario(|| {
            // The import itself runs `PyInit__bw_profile`: init budget
            // handed to the profiler, trace function installed. The
            // method table is asserted by name, the module doc by text
            // (`bw_profile.cpp:47-73,320`).
            assert!(run_simple(
                "import _bw_profile as p\n\
                 names = sorted(n for n in dir(p) if not n.startswith('__')\n\
                 or n in ('__onThreadStart', '__onThreadEnd'))\n\
                 global __probe\n\
                 __probe = \",\".join(names) + \"|\" + p.__doc__\n"
            ));
            assert_eq!(
                read_probe(),
                "__onThreadEnd,__onThreadStart,enable,getJsonDumpState,\
                 getJsonDumpStateValue,getJsonOutputFilePath,isEnabled,\
                 setJsonDumpCount,setJsonDumpDir,startJsonDump,tick\
                 |BigWorld bw_profile module"
            );
            // The two hooks sit on the module under their dunder names.
            assert!(run_simple(
                "import _bw_profile as p\n\
                 global __probe\n\
                 __probe = str(callable(p.__onThreadStart)) + str(callable(p.__onThreadEnd))\n"
            ));
            assert_eq!(read_probe(), "TrueTrue");
        });
    }

    #[test]
    #[ignore = "runs in its own process: see SCENARIOS"]
    fn scenario_enable_and_is_enabled_track_the_watcher_face() {
        interpreter_scenario(|| {
            // `(i == 0) ? false : true` (`bw_profile.cpp:157`): any nonzero
            // argument enables, zero disables; `isEnabled` reads the watcher
            // face (`bw_profile.cpp:165`). The middle readings pin the
            // reference's own quirk: `enable(false)` never clears
            // `isPendingActive_` (`profiler.cpp:2553-2560`), so a disable
            // requested while a pending activation sits outstanding reads
            // enabled until the tick that clears the flag.
            assert!(run_simple(
                "import _bw_profile as p\n\
                 r = []\n\
                 r.append(p.isEnabled())\n\
                 p.enable(1)\n\
                 r.append(p.isEnabled())\n\
                 p.enable(0)\n\
                 r.append(p.isEnabled())\n\
                 p.enable(5)\n\
                 r.append(p.isEnabled())\n\
                 p.tick()\n\
                 r.append(p.isEnabled())\n\
                 p.enable(0)\n\
                 r.append(p.isEnabled())\n\
                 global __probe\n\
                 __probe = \",\".join(str(x) for x in r)\n"
            ));
            assert_eq!(read_probe(), "0,1,1,1,1,0");
            // The last disable ran with no pending activation left, so the
            // off is the real state: the tick now early-returns instead of
            // re-activating.
            assert!(!global().watcher_enabled());
            global().tick();
            assert!(!global().watcher_enabled());
        });
    }

    #[test]
    #[ignore = "runs in its own process: see SCENARIOS"]
    fn scenario_argument_shapes_refuse_the_wrong_types() {
        interpreter_scenario(|| {
            // The `PyArg_ParseTuple` failures (`bw_profile.cpp:127,152,191,
            // 206,264`) raise TypeError before any profiler call.
            assert!(run_simple(
                "import _bw_profile as p\n\
                 def raises(fn, *args):\n    \
                 try:\n        fn(*args)\n        return 'none'\n    \
                 except TypeError:\n        return 'type'\n    \
                 except Exception as error:\n        return type(error).__name__\n\
                 r = [raises(p.enable, 'x'), raises(p.enable),\n    \
                 raises(p.__onThreadStart, 123),\n    \
                 raises(p.setJsonDumpCount, None),\n    \
                 raises(p.setJsonDumpDir, 5),\n    \
                 raises(p.getJsonDumpStateValue, 5)]\n\
                 global __probe\n\
                 __probe = \",\".join(r)\n"
            ));
            assert_eq!(read_probe(), "type,type,type,type,type,type");
        });
    }

    #[test]
    #[ignore = "runs in its own process: see SCENARIOS"]
    fn scenario_get_json_dump_state_value_maps_the_four_names() {
        interpreter_scenario(|| {
            assert!(run_simple(
                "import _bw_profile as p\n\
                 v = [p.getJsonDumpStateValue('JSON_DUMPING_INACTIVE'),\n    \
                 p.getJsonDumpStateValue('JSON_DUMPING_ACTIVE'),\n    \
                 p.getJsonDumpStateValue('JSON_DUMPING_COMPLETED'),\n    \
                 p.getJsonDumpStateValue('JSON_DUMPING_FAILED')]\n\
                 try:\n    \
                 p.getJsonDumpStateValue('NOPE')\n    \
                 err = 'no-raise'\n\
                 except RuntimeError as error:\n    \
                 err = str(error)\n\
                 global __probe\n\
                 __probe = ''.join(str(x) for x in v) + '|' + err\n"
            ));
            assert_eq!(
                read_probe(),
                "0123|getJsonDumpStateValue: Invalid dump state value NOPE"
            );
        });
    }

    #[test]
    #[ignore = "runs in its own process: see SCENARIOS"]
    fn scenario_set_json_dump_dir_checks_write_access() {
        // The embedded interpreter runs with `Py_NoSiteFlag`, so the
        // scenario cannot `import tempfile`: the writable directory is
        // built on the Rust side and spliced into the script.
        let good = std::env::temp_dir().join(format!(
            "boundless-bw-profile-dump-dir-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&good).expect("build the dump dir");
        let good = good.to_str().expect("a UTF-8 path");
        interpreter_scenario(|| {
            // A writable directory stores; an unreachable path raises
            // IOError with the constructed message
            // (`bw_profile.cpp:211-229`). The libc error text in the
            // middle is locale-dependent, so the assertion matches the
            // fixed halves around it.
            let source = format!(
                "import _bw_profile as p\n\
                 global __probe, __good\n\
                 p.setJsonDumpDir('{good}')\n\
                 try:\n    \
                 p.setJsonDumpDir('/nonexistent/bw-profile-dump-probe')\n    \
                 err = 'no-raise'\n\
                 except OSError as error:\n    \
                 err = str(error)\n\
                 __probe = err\n\
                 __good = '{good}'\n"
            );
            assert!(run_simple(&source));
            let probe = read_probe();
            assert!(
                probe.starts_with(
                    "setJsonDumpDir: Unable to write to JSON dump directory: \
                     /nonexistent/bw-profile-dump-probe: "
                ),
                "{probe}"
            );
            assert!(probe.ends_with('.'), "{probe}");
            // The refusal left the stored directory at the good path.
            assert_eq!(global().json_dump_dir(), good);
            assert_eq!(read_global(b"__good\0"), good);
            let _ = std::fs::remove_dir_all(good);
        });
    }

    /// A `__main__` global read back as a string. The key is a byte
    /// sequence the two probe names in this file, NUL-terminated the way
    /// `PyDict_GetItemString` requires.
    fn read_global(name: &[u8]) -> String {
        // SAFETY: the interpreter is up.
        unsafe {
            let main = capi::PyImport_AddModule(c"__main__".as_ptr());
            let dictionary = capi::PyModule_GetDict(main);
            let value = capi::PyDict_GetItemString(dictionary, name.as_ptr() as *const c_char);
            let text = capi::PyObject_Str(value);
            let rendered = capi::to_string(text);
            capi::dec_ref(text);
            rendered
        }
    }

    #[test]
    #[ignore = "runs in its own process: see SCENARIOS"]
    fn scenario_the_json_faces_report_the_armed_state() {
        interpreter_scenario(|| {
            // Fresh module: the state is inactive and no file has been
            // written; `startJsonDump` only arms the watcher
            // (`bw_profile.cpp:181`, `profiler.cpp` watcherSetDumpProfile),
            // and the dump writer itself is BW-12-03-03.
            assert!(run_simple(
                "import _bw_profile as p\n\
                 before = str(p.getJsonDumpState()) + '|' + p.getJsonOutputFilePath()\n\
                 p.startJsonDump()\n\
                 p.setJsonDumpCount(3)\n\
                 global __probe\n\
                 __probe = before + '|' + str(p.getJsonDumpState())\n"
            ));
            assert_eq!(read_probe(), "0||0");
            assert!(global().watcher_get_dump_profile());
            assert_eq!(global().json_dump_count_snapshot(), 3);
        });
    }

    #[test]
    #[ignore = "runs in its own process: see SCENARIOS"]
    fn scenario_python_calls_become_category_python_entries() {
        interpreter_scenario(|| {
            // The callback face (`bw_profile.cpp:76-120`): the module
            // calls are C events (their descriptor names), `work` is a
            // Python frame (its `co_name`), and `len` nests inside it.
            //
            // Two reference gates shape the sequence: `addEntry` drops
            // everything recorded while the profiler is inactive — a
            // pending activation does not count (`profiler.ipp:30-33`) —
            // so `enable` is followed by its activation tick before the
            // interesting calls; and the first tick after `enable` only
            // activates, so the LAST tick is the one that swaps and
            // processes the frame.
            assert!(run_simple(
                "import _bw_profile as p\n\
                 p.enable(1)\n\
                 p.tick()\n\
                 p.__onThreadStart('MainThread')\n\
                 def work():\n    \
                 return len([1, 2])\n\
                 work()\n\
                 p.tick()\n\
                 def boom():\n    \
                 raise ValueError\n\
                 try:\n    \
                 boom()\n\
                 except ValueError:\n    \
                 pass\n\
                 dict.fromkeys([])\n\
                 global __probe\n\
                 __probe = 'ok'\n\
                 p.__onThreadEnd()\n"
            ));
            assert_eq!(read_probe(), "ok");
            let profiler = global();
            // The processed frame: the activation tick's own C return,
            // the hook pair, the work pair with the len child, and the
            // closing tick's C call.
            assert_eq!(profiler.final_entry_count(), 8);
            let expected: [(&str, EventType); 8] = [
                ("tick", EventType::End),
                ("__onThreadStart", EventType::Start),
                ("__onThreadStart", EventType::End),
                ("work", EventType::Start),
                ("len", EventType::Start),
                ("len", EventType::End),
                ("work", EventType::End),
                ("tick", EventType::Start),
            ];
            for (index, (name, event)) in expected.iter().enumerate() {
                let entry = profiler.final_entry(index as i32).expect("entry");
                assert_eq!(entry.name_str(), *name, "entry {index}");
                assert_eq!(entry.event, *event, "entry {index}");
                assert_eq!(entry.category, CATEGORY_PYTHON, "entry {index}");
                assert_eq!(entry.thread_idx, 0, "entry {index}");
            }
            // init registered the importing thread as "MainThread"
            // (`profiler.cpp:1049`); the hook's re-add only re-pins it.
            assert_eq!(profiler.get_thread_name(0).as_deref(), Some("MainThread"));
            // Display build: the hook, work (with the len child) and the
            // unclosed tick start clamped at the frame end; the leading
            // tick END hit an empty stack and built nothing.
            assert_eq!(profiler.display_entry_count(), 4);
            // Entries recorded after the swap: the closing tick's own C
            // return, the boom pair (its raise fires no profile event at
            // all: Python-frame EXCEPTION events go to trace, not profile,
            // callbacks — verified on the pinned interpreter), the fromkeys
            // pair (the bound builtin: 3.13 binds `dict.fromkeys` to a
            // PyCFunction before emitting the event, so the tracer never
            // sees the raw classmethod descriptor), and `__onThreadEnd`'s
            // C call (its return ran after the trace function was cleared)
            // wait in the next buffer.
            assert_eq!(profiler.recording_entry_count(), 6);
            // The profiler is active, so `__onThreadEnd`'s removal is
            // deferred to a frame end that never comes: the thread is
            // still registered under its name.
            assert_eq!(profiler.num_threads(), 1);
            // The run helper's failure faces: an interior NUL fails the
            // CString conversion, and an uncaught script error fails the
            // run (`PyRun_SimpleString` prints and clears the error).
            assert!(!run_simple("x\0=1"));
            assert!(!run_simple("raise NameError('probe')\n"));
        });
    }

    #[test]
    #[ignore = "runs in its own process: see SCENARIOS"]
    fn scenario_on_thread_end_unregisters_and_untraces() {
        interpreter_scenario(|| {
            // `__onThreadEnd` drops the thread and clears the trace
            // function (`bw_profile.cpp:139-145`); the module-frame code
            // after it records nothing further.
            assert!(run_simple(
                "import _bw_profile as p\n\
                 p.__onThreadEnd()\n\
                 global __probe\n\
                 __probe = str(p.isEnabled())\n"
            ));
            assert_eq!(read_probe(), "0");
            // Nothing was recorded at all: the profiler was never
            // enabled, and `addEntry` drops entries while inactive
            // (`profiler.ipp:30-33`) — the hook's own C call included.
            assert_eq!(global().recording_entry_count(), 0);
            // init's "MainThread" registration is gone.
            assert_eq!(global().num_threads(), 0);
            assert_eq!(global().get_thread_name(0), None);
        });
    }

    /// Building the interpreter around every scenario is a
    /// one-process-at-a-time affair; the parent only dispatches.
    #[test]
    fn the_scenarios_run() {
        for scenario in SCENARIOS {
            run_scenario(scenario);
        }
    }

    #[test]
    fn the_method_table_matches_the_reference() {
        // bw_profile.cpp:47-73: names, flags and docs in order, terminated
        // by the null row.
        let table = METHOD_TABLE.0;
        assert_eq!(table.len(), 12);
        let names: [&CStr; 11] = [
            c"__onThreadStart",
            c"__onThreadEnd",
            c"enable",
            c"isEnabled",
            c"tick",
            c"startJsonDump",
            c"setJsonDumpCount",
            c"setJsonDumpDir",
            c"getJsonDumpState",
            c"getJsonDumpStateValue",
            c"getJsonOutputFilePath",
        ];
        let flags: [c_int; 11] = [
            capi::METH_VARARGS,
            METH_NOARGS,
            capi::METH_VARARGS,
            capi::METH_VARARGS,
            METH_NOARGS,
            METH_NOARGS,
            capi::METH_VARARGS,
            capi::METH_VARARGS,
            METH_NOARGS,
            capi::METH_VARARGS,
            capi::METH_VARARGS,
        ];
        for (index, name) in names.iter().enumerate() {
            assert_eq!(unsafe { CStr::from_ptr(table[index].ml_name) }, *name);
            assert_eq!(table[index].ml_flags, flags[index]);
            assert!(!table[index].ml_doc.is_null());
            assert!(table[index].ml_meth.is_some());
        }
        assert!(table[11].ml_name.is_null());
        assert!(table[11].ml_meth.is_none());
        // The docstrings the reference carries verbatim, including the
        // trailing spaces (`bw_profile.cpp:62,64,71`).
        let doc = |index: usize| -> String {
            unsafe { CStr::from_ptr(table[index].ml_doc) }
                .to_string_lossy()
                .into_owned()
        };
        assert_eq!(doc(0), "Thread start hook; begins profiling a thread");
        assert_eq!(doc(1), "Thread end hook; finishes profiling a thread");
        assert_eq!(doc(2), "Enable or disable performance profiling");
        assert_eq!(
            doc(3),
            "Returns whether performance profiling currently running"
        );
        assert_eq!(doc(4), "Profiler Tick event");
        assert_eq!(doc(5), "Create a json dump file");
        assert_eq!(doc(6), "Set the Json Dump Count ");
        assert_eq!(doc(7), "Set the Json Dump Dir ");
        assert_eq!(doc(8), "Get the Json Dump State");
        assert_eq!(
            doc(9),
            "Get the equivalent dump state enum value of the argument string"
        );
        assert_eq!(doc(10), "Get the latest json output file path ");
    }

    #[test]
    fn the_trace_constants_match_the_interpreter() {
        // `Include/cpython/ceval.h`, `enum _pytrace_t`: the values CPython
        // passes as the action argument.
        assert_eq!(PyTrace_CALL, 0);
        assert_eq!(PyTrace_RETURN, 3);
        assert_eq!(PyTrace_C_CALL, 4);
        assert_eq!(PyTrace_C_RETURN, 5);
        assert_eq!(PyTrace_C_EXCEPTION, 6);
    }

    #[test]
    fn the_c_function_view_reads_the_descriptor_at_the_pinned_offset() {
        // `m_ml` sits right after the two-word object header
        // (`Include/methodobject.h:63`); the tracer's ml_name read depends
        // on the offset, so it is pinned like python-module's layouts.
        assert_eq!(std::mem::offset_of!(PyCFunctionObject, m_ml), 16);
        assert_eq!(std::mem::size_of::<PyCFunctionObject>(), 24);
    }

    #[test]
    fn the_module_def_matches_the_interpreter_header() {
        // The layouts python-module compiled a probe against (3.14): a
        // drift here segfaults the import, so they are pinned.
        assert_eq!(std::mem::size_of::<PyModuleDef>(), 104);
        assert_eq!(std::mem::size_of::<PyModuleDefBase>(), 40);
        assert_eq!(std::mem::offset_of!(PyModuleDef, m_base), 0);
        assert_eq!(std::mem::offset_of!(PyModuleDef, m_name), 40);
        assert_eq!(std::mem::offset_of!(PyModuleDef, m_doc), 48);
        assert_eq!(std::mem::offset_of!(PyModuleDef, m_size), 56);
        assert_eq!(std::mem::offset_of!(PyModuleDef, m_methods), 64);
        assert_eq!(std::mem::offset_of!(PyModuleDef, m_slots), 72);
        assert_eq!(std::mem::offset_of!(PyModuleDef, m_traverse), 80);
        assert_eq!(std::mem::offset_of!(PyModuleDef, m_clear), 88);
        assert_eq!(std::mem::offset_of!(PyModuleDef, m_free), 96);
        assert_eq!(std::mem::offset_of!(PyModuleDefBase, m_init), 16);
        assert_eq!(std::mem::offset_of!(PyModuleDefBase, m_index), 24);
        assert_eq!(std::mem::offset_of!(PyModuleDefBase, m_copy), 32);
        // The reference's definition (`bw_profile.cpp:317-324`): single
        // phase, the method table attached. The pointer check runs at
        // runtime — const contexts promote `as_ptr()` into per-use
        // allocations whose addresses differ, but the loaded static's
        // relocation and the table's own address agree.
        assert_eq!(MODULE_DEF.m_size, -1);
        assert_eq!(
            MODULE_DEF.m_methods as *const PyMethodDef,
            METHOD_TABLE.0.as_ptr()
        );
    }

    #[test]
    fn the_dump_state_names_resolve_to_the_enums() {
        // bw_profile.cpp:269-284, read through the profiler's enum rather
        // than a hardcoded copy.
        assert_eq!(dump_state_value("JSON_DUMPING_INACTIVE"), Some(0));
        assert_eq!(dump_state_value("JSON_DUMPING_ACTIVE"), Some(1));
        assert_eq!(dump_state_value("JSON_DUMPING_COMPLETED"), Some(2));
        assert_eq!(dump_state_value("JSON_DUMPING_FAILED"), Some(3));
        assert_eq!(dump_state_value("JSON_DUMPING_BOGUS"), None);
        assert_eq!(dump_state_value(""), None);
        assert_eq!(dump_state_value("json_dumping_active"), None);
    }

    #[test]
    fn the_dump_dir_messages_are_built_like_the_reference() {
        // bw_profile.cpp:213-217 then :223-224.
        let message = dump_dir_error_message("/tmp/dumps", "Permission denied");
        assert_eq!(
            message,
            "Unable to write to JSON dump directory: /tmp/dumps: Permission denied."
        );
        assert_eq!(
            dump_dir_exception_message(&message),
            "setJsonDumpDir: Unable to write to JSON dump directory: \
             /tmp/dumps: Permission denied."
        );
    }

    #[test]
    fn a_c_message_truncates_at_an_interior_nul() {
        // The reference hands `BW::string::c_str()` to the C API, so an
        // interior NUL truncates the C view at that byte; the port
        // reproduces the truncation instead of refusing the string.
        let encoded = c_message("before\0after");
        assert_eq!(encoded.to_bytes(), b"before");
        assert_eq!(c_message("").to_bytes(), b"");
        assert_eq!(c_message("plain").to_bytes(), b"plain");
    }
}
