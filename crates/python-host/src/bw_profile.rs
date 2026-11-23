//! The `_bw_profile` module face (BW-12-03-02): the start/stop interface
//! of `server/tools/bw_profile/bw_profile.cpp` — the module methods the
//! scripts drive (`enable`, `isEnabled`, `tick`, `__onThreadStart`,
//! `__onThreadEnd`, `bw_profile.cpp:31-67`), the trace callback that
//! turns Python call events into profiler entries (`profileFunc`,
//! `bw_profile.cpp:76-121`) and the module's creation work
//! (`PyInit__bw_profile`, `bw_profile.cpp:322-342`).
//!
//! The JSON dump methods — `startJsonDump`, `setJsonDumpCount`,
//! `setJsonDumpDir`, `getJsonDumpState`, `getJsonDumpStateValue`,
//! `getJsonOutputFilePath` — are BW-12-03-03's and stay off the table
//! until that item delivers them.
//!
//! The reference builds the extension module into its profiling binaries
//! and scripts reach it with `import _bw_profile`, whose import runs
//! `PyInit__bw_profile`: the fixed 128MB profiler budget (its own TODO
//! keeps it configurable, `bw_profile.cpp:329-331`), the module object
//! and the trace callback armed. The port's host is one binary, so the
//! module is created at interpreter init — pre-registered in
//! `sys.modules`, which makes a script's `import _bw_profile` find it —
//! and the same import-time work runs there. The `profiler/enabled`
//! watcher `Profiler::init` registers (`profiler.cpp:896-899`) mounts
//! from here too, the watcher addressing the global the way the
//! reference's `MF_WATCH` does.

use std::ffi::CStr;
use std::os::raw::{c_char, c_int};

use profile::{
    EventCategory, EventType, MAXIMUM_NUMBER_OF_THREADS, g_profiler, mount_enabled_watcher,
    our_thread_id,
};

use crate::capi;

/// `MAX_PROFILER_MEMORY_BYTES` (`bw_profile.cpp:330`): the fixed budget
/// the reference's TODO marks "make this configurable".
const MAX_PROFILER_MEMORY_BYTES: i32 = 128 * 1024 * 1024;

/// `PyInit__bw_profile` (`bw_profile.cpp:322-342`) at the port's import
/// moment — interpreter init: the profiler budget, the enabled watcher
/// and the module object with its method table, then the trace callback.
///
/// # Safety
///
/// The interpreter must be up and the caller must own the GIL.
pub unsafe fn install() {
    // SAFETY: the interpreter is up and this thread owns the GIL.
    unsafe {
        g_profiler()
            .lock()
            .expect("the profiler lock")
            .init(MAX_PROFILER_MEMORY_BYTES, MAXIMUM_NUMBER_OF_THREADS);
        mount_enabled_watcher();

        // `PyImport_AddModule` is the port's `PyModule_Create` here: the
        // module enters `sys.modules` and a script's import finds it
        // without running any init of its own.
        let module = capi::PyImport_AddModule(c"_bw_profile".as_ptr());
        if module.is_null() {
            capi::clear_error();
            return;
        }
        // `m_doc` (`bw_profile.cpp:315`).
        let doc = capi::PyUnicode_FromString(c"BigWorld bw_profile module".as_ptr());
        if !doc.is_null() {
            capi::PyObject_SetAttrString(module, c"__doc__".as_ptr(), doc);
            capi::dec_ref(doc);
        }

        let methods: &'static mut [capi::PyMethodDef] = Box::leak(
            vec![
                capi::PyMethodDef {
                    ml_name: c"__onThreadStart".as_ptr(),
                    ml_meth: Some(method_on_thread_start),
                    ml_flags: capi::METH_VARARGS,
                    ml_doc: c"Thread start hook; begins profiling a thread".as_ptr(),
                },
                capi::PyMethodDef {
                    ml_name: c"__onThreadEnd".as_ptr(),
                    ml_meth: Some(method_on_thread_end),
                    ml_flags: capi::METH_NOARGS,
                    ml_doc: c"Thread end hook; finishes profiling a thread".as_ptr(),
                },
                capi::PyMethodDef {
                    ml_name: c"enable".as_ptr(),
                    ml_meth: Some(method_enable),
                    ml_flags: capi::METH_VARARGS,
                    ml_doc: c"Enable or disable performance profiling".as_ptr(),
                },
                capi::PyMethodDef {
                    ml_name: c"isEnabled".as_ptr(),
                    ml_meth: Some(method_is_enabled),
                    ml_flags: capi::METH_VARARGS,
                    ml_doc: c"Returns whether performance profiling currently running".as_ptr(),
                },
                capi::PyMethodDef {
                    ml_name: c"tick".as_ptr(),
                    ml_meth: Some(method_tick),
                    ml_flags: capi::METH_NOARGS,
                    ml_doc: c"Profiler Tick event".as_ptr(),
                },
                capi::PyMethodDef::END,
            ]
            .into_boxed_slice(),
        );
        // SAFETY: the module is live and the table is a terminated static
        // array of `PyMethodDef`.
        if capi::PyModule_AddFunctions(module, methods.as_ptr()) != 0 {
            // The reference has no failure arm for this; the script_events
            // face logs through the same print-and-clear.
            capi::PyErr_PrintEx(0);
            capi::clear_error();
        }

        // The `PyInit` tail (`bw_profile.cpp:340-342`): the trace
        // callback is armed as soon as the module exists, the way an
        // import arms it in the reference.
        capi::PyEval_SetProfile(Some(profile_func), std::ptr::null_mut());
    }
}

/// `profileFunc` (`bw_profile.cpp:76-121`): Python call events become
/// profiler records — the frame's code name for `PyTrace_CALL`/`RETURN`,
/// the C function's name for `PyTrace_C_CALL`/`C_RETURN`/`C_EXCEPTION`,
/// everything through `CATEGORY_PYTHON`. The reference reads
/// `PyCFunctionObject::m_ml->ml_name` directly; the port reads the same
/// string through the object protocol (`__name__`), the way the
/// reference's own 3.13 migration moved the frame's `co_name` read
/// behind `PyFrame_GetCode` (`bw_profile.cpp:79-82`).
///
/// # Safety
///
/// The caller is the interpreter: the GIL is held, `frame` is the live
/// frame of the event (or an arbitrary object for the C events) and
/// `arg` the event's argument. Any null checks are the reference's own.
///
/// The callback is fenced: the reference ran under old CPython's
/// `call_trace_protected`, which stashed a pending exception
/// (`PyErr_Fetch`) around the callback and restored it after, while the
/// 3.13+ monitoring shim drives its legacy handlers unfenced. The port
/// stashes with the modern fetch at entry; on the way out a stashed
/// exception goes back through the error return — the shim's
/// `call_profile_func` (`legacy_tracing.c:44-47`) only hands an
/// exception onward when the callback reports failure, and a zero
/// return with the indicator set is what trips
/// `returned a result with an exception set` on the handler. A genuine
/// unwind keeps its own exception and nothing chained onto it. A stale
/// pending error — the reference's unchecked int reads leave one behind
/// — is defused at the read site instead (`math::coerce_float`), since
/// no callback return can hide an indicator the shim checks.
unsafe extern "C" fn profile_func(
    _self: *mut capi::PyObject,
    frame: *mut capi::PyObject,
    action: c_int,
    arg: *mut capi::PyObject,
) -> c_int {
    // SAFETY: the interpreter is calling this callback with the GIL
    // held; the fetch is the fence's stash.
    let saved = unsafe { capi::PyErr_GetRaisedException() };
    // SAFETY: the interpreter is running this callback with the GIL held.
    unsafe {
        let mut profiler = g_profiler().lock().expect("the profiler lock");
        match action {
            capi::PyTrace_CALL | capi::PyTrace_RETURN => {
                let code = capi::PyFrame_GetCode(frame);
                let name = capi::PyObject_GetAttrString(code, c"co_name".as_ptr());
                let text: *const c_char = if name.is_null() {
                    // The reference's `<unknown>` fallback
                    // (`bw_profile.cpp:85-86`).
                    c"<unknown>".as_ptr()
                } else {
                    let rendered = capi::PyUnicode_AsUTF8(name);
                    if rendered.is_null() {
                        // `addEntry` copies `text ? text : ""`
                        // (`profiler.ipp:31`).
                        c"".as_ptr()
                    } else {
                        rendered
                    }
                };
                profiler.add_entry(
                    CStr::from_ptr(text).to_str().unwrap_or(""),
                    if action == capi::PyTrace_CALL {
                        EventType::Start
                    } else {
                        EventType::End
                    },
                    0,
                    EventCategory::Python,
                );
                if !name.is_null() {
                    capi::dec_ref(name);
                }
            }
            capi::PyTrace_C_CALL | capi::PyTrace_C_RETURN | capi::PyTrace_C_EXCEPTION
                if !arg.is_null() && capi::py_cfunction_check(arg) =>
            {
                let name = capi::PyObject_GetAttrString(arg, c"__name__".as_ptr());
                if !name.is_null() {
                    // SAFETY: a live unicode from the attribute read.
                    let rendered = capi::PyUnicode_AsUTF8(name);
                    let text = if rendered.is_null() {
                        c"".as_ptr()
                    } else {
                        rendered
                    };
                    profiler.add_entry(
                        CStr::from_ptr(text).to_str().unwrap_or(""),
                        if action == capi::PyTrace_C_CALL {
                            EventType::Start
                        } else {
                            EventType::End
                        },
                        0,
                        EventCategory::Python,
                    );
                    capi::dec_ref(name);
                } else {
                    capi::clear_error();
                }
            }
            // `EXCEPTION`, `LINE` and `OPCODE` fall through the
            // reference's switch unhandled.
            _ => {}
        }
    }
    if saved.is_null() {
        // Nothing was pending at entry: any error the walk left behind
        // is this callback's own garbage, not the script's — the same
        // cleared slate `call_trace_protected` handed back.
        // SAFETY: the interpreter is up and this thread owns the GIL.
        unsafe { capi::clear_error() };
        0
    } else {
        // The exception belongs to the unwinding script, not to this
        // callback: the shim hands it back only through the error
        // return (`call_profile_func`, `legacy_tracing.c:44-47` —
        // `if (err) return NULL`), because a zero return with the
        // indicator set is what trips
        // `returned a result with an exception set`. The same object
        // comes back, so the script sees its own exception and nothing
        // chained onto it.
        // SAFETY: the saved exception is owned and moves back to the
        // interpreter, replacing anything the walk left pending.
        unsafe { capi::PyErr_SetRaisedException(saved) };
        -1
    }
}

/// `_bw_profile___onThreadStart` (`bw_profile.cpp:123-137`): register
/// the calling thread under its name and arm the trace callback.
///
/// # Safety
///
/// The interpreter is calling the module method with the GIL held.
unsafe extern "C" fn method_on_thread_start(
    _module: *mut capi::PyObject,
    args: *mut capi::PyObject,
) -> *mut capi::PyObject {
    let mut thread_name: *const c_char = std::ptr::null();
    // SAFETY: the caller owns the GIL and the parse target is a live
    // local of C ABI size.
    if unsafe { capi::PyArg_ParseTuple(args, c"s".as_ptr(), &mut thread_name) } == 0 {
        // The parser left its own `TypeError` pending.
        return std::ptr::null_mut();
    }
    // SAFETY: the interpreter is up and this thread owns the GIL; the
    // name pointer is a NUL-terminated C string the parser handed back.
    unsafe {
        let name = if thread_name.is_null() {
            ""
        } else {
            CStr::from_ptr(thread_name).to_str().unwrap_or("")
        };
        g_profiler()
            .lock()
            .expect("the profiler lock")
            .add_this_thread(name);
        capi::PyEval_SetProfile(Some(profile_func), std::ptr::null_mut());
    }
    capi::py_none()
}

/// `_bw_profile___onThreadEnd` (`bw_profile.cpp:139-146`): drop the
/// calling thread's registration and the trace callback with it.
///
/// # Safety
///
/// The interpreter is calling the module method with the GIL held.
unsafe extern "C" fn method_on_thread_end(
    _module: *mut capi::PyObject,
    _args: *mut capi::PyObject,
) -> *mut capi::PyObject {
    // SAFETY: the interpreter is up and this thread owns the GIL.
    unsafe {
        g_profiler()
            .lock()
            .expect("the profiler lock")
            .remove_thread(our_thread_id());
        capi::PyEval_SetProfile(None, std::ptr::null_mut());
    }
    capi::py_none()
}

/// `_bw_profile_enable` (`bw_profile.cpp:148-162`): any nonzero
/// argument enables.
///
/// # Safety
///
/// The interpreter is calling the module method with the GIL held.
unsafe extern "C" fn method_enable(
    _module: *mut capi::PyObject,
    args: *mut capi::PyObject,
) -> *mut capi::PyObject {
    let mut enabled: c_int = 0;
    // SAFETY: the caller owns the GIL and the parse target is a live
    // local of C ABI size.
    if unsafe { capi::PyArg_ParseTuple(args, c"i".as_ptr(), &mut enabled) } == 0 {
        return std::ptr::null_mut();
    }
    g_profiler()
        .lock()
        .expect("the profiler lock")
        .enable(enabled != 0);
    capi::py_none()
}

/// `_bw_profile_isEnabled` (`bw_profile.cpp:163-170`): the
/// `watcherEnabled` read — pending or active. The method takes `args`
/// the reference's table declares (`METH_VARARGS`, `bw_profile.cpp:50`)
/// and ignores them.
///
/// # Safety
///
/// The interpreter is calling the module method with the GIL held.
unsafe extern "C" fn method_is_enabled(
    _module: *mut capi::PyObject,
    _args: *mut capi::PyObject,
) -> *mut capi::PyObject {
    let status = i64::from(
        g_profiler()
            .lock()
            .expect("the profiler lock")
            .watcher_enabled(),
    );
    // SAFETY: the interpreter is up and this thread owns the GIL.
    unsafe { capi::PyLong_FromLong(status) }
}

/// `_bw_profile_tick` (`bw_profile.cpp:171-177`).
///
/// # Safety
///
/// The interpreter is calling the module method with the GIL held.
unsafe extern "C" fn method_tick(
    _module: *mut capi::PyObject,
    _args: *mut capi::PyObject,
) -> *mut capi::PyObject {
    g_profiler().lock().expect("the profiler lock").tick();
    capi::py_none()
}
