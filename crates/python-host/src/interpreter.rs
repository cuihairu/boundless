//! `Script::init`/`Script::fini` -- the interpreter lifecycle
//! (`lib/pyscript/script.cpp`).
//!
//! The reference sequence and its Rust mapping:
//!
//! | Reference step (`Script::init`) | Port |
//! | --- | --- |
//! | re-init returns true immediately (`s_isInitalised`) | [`Interpreter::init`] returns `Ok` |
//! | `Py_FrozenFlag = 1`, `Py_NoSiteFlag = 1`, `Py_IgnoreEnvironmentFlag = 1` | set before `Py_Initialize` |
//! | `Py_Initialize()` | same |
//! | `PySys_SetObject("argv", pArgvList)`, only when `g_scriptArgc` | `InitConfig::argv` |
//! | `PySys_SetObject("path", sysPaths.pathAsObject())` | `InitConfig::import_paths` |
//! | `gc.disable()` (`Script::disablePythonGarbage`) | `disable_garbage` |
//! | `s_isInitalised = true` | [`Interpreter::is_initialized`] |
//! | `Script::fini` | [`Interpreter::fini`] |
//!
//! `Script::fini` order: drain `PyGC_Collect` until a pass collects nothing
//! more, blank the `__main__` module dictionary (every attribute except
//! `__builtins__` becomes `None`, then `sys.modules["__main__"]` becomes
//! `None` -- the reference re-implements the removed `_PyModule_Clear`
//! inline), `Py_Finalize`, then the flag.
//!
//! The stdout redirection, module registration and the fini-time jobs arrive
//! with their own BW-03-01 items.

use std::ffi::CString;
use std::sync::atomic::{AtomicBool, AtomicPtr, Ordering};

use super::binding;
use super::capi::{self, PyObject};
use super::gil;
use super::import_paths::ImportPaths;
use super::init_time_job;
use super::keepalive;
use super::math;
use super::output_writer;
use super::wrapper;

static INITIALIZED: AtomicBool = AtomicBool::new(false);

/// `s_pOurInitTimeModules`: a copy of `sys.modules` taken once init has
/// registered every native module. Reload swaps it back into a fresh
/// subinterpreter (`Script::createInterpreter`), so the table travels with
/// the interpreter.
static INIT_TIME_MODULES: AtomicPtr<PyObject> = AtomicPtr::new(std::ptr::null_mut());

/// Why `Script::init` could not complete.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InitError {
    /// `Py_IsInitialized` is still 0 after `Py_Initialize`: the interpreter
    /// did not come up.
    InitializeFailed,
    /// The `sys.argv` list could not be built (a `PyList_New` or
    /// `PyList_SetItem` failure in the reference's argv loop).
    ArgvFailed,
    /// `PySys_SetObject("path", ...)` refused the list. The reference logs
    /// `"Script::init: Unable to assign sys.path"` and returns false.
    PathFailed,
    /// `gc` could not be imported. The reference clears the import error and
    /// carries on with collection enabled; the port reports it instead so a
    /// mis-built runtime is visible.
    GarbageCollectionUnavailable,
    /// The stdio redirect could not be installed (`new ScriptOutputWriter()`
    /// at `script.cpp:531`, `MF_VERIFY`'d there; the port reports instead of
    /// aborting).
    StdioFailed,
    /// `import bw_site` failed, the reference's
    /// `"Script::init: Unable to import bw_site"` path (`script.cpp:617`),
    /// which also returns false.
    BwSiteImportFailed,
}

/// The `Script::init` arguments BW-03-01 owns.
#[derive(Clone, Debug, Default)]
pub struct InitConfig {
    /// `g_scriptArgv`. An empty list leaves `sys.argv` at the interpreter
    /// default, exactly like the reference's `if (g_scriptArgc)` guard.
    pub argv: Vec<String>,
    /// The `PyImportPaths` the app passes to `Script::init`. Always becomes
    /// `sys.path` -- an empty collection included, since the reference replaces
    /// the interpreter default rather than extending it.
    pub import_paths: ImportPaths,
    /// The `componentName` argument of `Script::init`: one of `cell`, `base`,
    /// `client`, `database`, `bot` or `editor`. It becomes the
    /// `BigWorld.component` attribute every script reads.
    pub component: String,
}

/// `Script`: the embedded interpreter handle.
#[derive(Debug)]
pub struct Interpreter;

impl Interpreter {
    /// `Script::init`. Idempotent on success: a second call while the
    /// interpreter is up returns `Ok` without touching anything, matching
    /// `if (s_isInitalised) return true;`.
    ///
    /// One interpreter per process. CPython 3.13 and later abort on a second
    /// `Py_Initialize` after `Py_Finalize` (`Fatal Python error:
    /// _PyImport_Init: global import state already initialized`), and the
    /// reference never re-initialises either: it swaps subinterpreters for
    /// reload (`Script::createInterpreter`) and finalises once at process end.
    /// Restarting is therefore a process-level operation, which is also what
    /// the BW-03-01-01 acceptance measures.
    pub fn init(config: &InitConfig) -> Result<Interpreter, InitError> {
        if INITIALIZED.load(Ordering::SeqCst) {
            return Ok(Interpreter);
        }
        // SAFETY: the interpreter is not up, so the pre-init globals are
        // writable and every call below runs while it is coming up.
        unsafe {
            // The reference sets these three legacy globals and calls plain
            // `Py_Initialize`; migrating to `PyConfig` is an open,
            // behaviour-level change in the 3.13 migration ledger, so the
            // port keeps the globals too.
            capi::Py_FrozenFlag = 1;
            capi::Py_NoSiteFlag = 1;
            capi::Py_IgnoreEnvironmentFlag = 1;
            capi::Py_Initialize();
            if capi::Py_IsInitialized() == 0 {
                return Err(InitError::InitializeFailed);
            }
            // `s_pMainThreadState = PyThreadState_Get(); s_defaultContext =
            // s_pMainThreadState;` (`script.cpp:603-604`), captured on the
            // thread that brings the interpreter up.
            let _ = gil::init_thread();
        }
        // `new ScriptOutputWriter()` sits right after `Py_Initialize`
        // (`script.cpp:531`), before argv: everything the rest of init logs
        // already lands in the script log entry.
        if !output_writer::install() {
            return Err(InitError::StdioFailed);
        }
        // The reference order from script.cpp: argv, sys.path, init-time
        // jobs, gc.disable, the BigWorld module, the module-table snapshot.
        let mut error = None;
        if !config.argv.is_empty() && !set_argv(&config.argv) {
            error = Some(InitError::ArgvFailed);
        }
        if error.is_none() && !set_import_paths(&config.import_paths) {
            error = Some(InitError::PathFailed);
        }
        if error.is_none() {
            // `runInitTimeJobs()`: module registrations made before init.
            // A spent registry logs and carries on in the reference, so init
            // does the same; through `init`'s own guard a second call is
            // unreachable.
            let _ = init_time_job::run_init_time_jobs();
        }
        if error.is_none() && !disable_garbage() {
            error = Some(InitError::GarbageCollectionUnavailable);
        }
        if error.is_none() {
            // `TempPyVectorInit` (`script_math.cpp:3053-3067`) is a rung-0
            // init-time job: the Math module and its classes exist before
            // the BigWorld module is built, and the module-table snapshot
            // sees Math either way.
            math::install();
            // `ScriptModule::getOrCreate("BigWorld")` with the `component`
            // attribute, then the `s_pOurInitTimeModules` snapshot.
            create_big_world_module(&config.component);
            take_modules_snapshot();
        }
        if let Some(error) = error {
            // Unwind back to the pre-init state so a retry is not refused by
            // the `s_isInitalised` guard.
            // SAFETY: the interpreter is up and this thread owns the GIL.
            unsafe {
                capi::Py_Finalize();
            }
            return Err(error);
        }
        INITIALIZED.store(true, Ordering::SeqCst);
        // `ScriptModule::import("bw_site", ...)` at `script.cpp:617`, the
        // tail of init: failure reports `BwSiteImportFailed` and unwinds
        // (the reference returns false with `s_isInitalised` left set -- the
        // port rolls back instead, consistent with its other failure arms).
        if !import_bw_site() {
            INITIALIZED.store(false, Ordering::SeqCst);
            // SAFETY: the interpreter is up and this thread owns the GIL.
            unsafe {
                capi::Py_Finalize();
            }
            return Err(InitError::BwSiteImportFailed);
        }
        // `initExceptionHook` is per-process code in the reference
        // (`cellapp.cpp:3622` et al.), called after `Script::init` returns;
        // folded into init until a process layer exists. The reference
        // ignores `PySys_SetObject`'s result here and so does the port.
        output_writer::install_exception_hook();
        Ok(Interpreter)
    }

    /// `Script::isInitalised` (the reference's spelling).
    pub fn is_initialized() -> bool {
        INITIALIZED.load(Ordering::SeqCst) && !capi::is_finalized()
    }

    /// `Script::isFinalised`.
    pub fn is_finalized() -> bool {
        capi::is_finalized()
    }

    /// `s_pOurInitTimeModules`: the borrowed module-table snapshot taken at
    /// init time, or null before init and after `Script::fini`.
    pub fn init_time_modules() -> *mut PyObject {
        INIT_TIME_MODULES.load(Ordering::SeqCst)
    }

    /// `PyRun_SimpleString`: the reference returns 0 on success.
    pub fn run_simple_string(&self, source: &str) -> bool {
        let Ok(encoded) = CString::new(source) else {
            return false;
        };
        // SAFETY: the interpreter is up and this thread owns the GIL.
        unsafe { capi::PyRun_SimpleString(encoded.as_ptr()) == 0 }
    }

    /// `Script::fini` (script.cpp:659): the shutdown order the port follows.
    ///
    /// The reference stops its background-task managers first ("they might be
    /// loading PyObjects or have script callbacks"), finalises the Pickler,
    /// then drains the fini-time jobs -- `Personality::onFini` rides the last
    /// rung -- before dropping the init-time module snapshot, the writer, the
    /// collected cycles, `__main__`, and finally CPython itself. The port owns
    /// no task managers or Pickler yet, so the stop-the-background-work step
    /// belongs to the process layer that drives `fini`, and this method covers
    /// the script side: jobs, snapshot, cycles, module clear, `Py_Finalize`.
    /// Unlike the reference, the port always finalises; the reference can skip
    /// `Py_Finalize` (client-module builds) and still mark itself finalised.
    pub fn fini(&self) {
        if !INITIALIZED.load(Ordering::SeqCst) || capi::is_finalized() {
            return;
        }
        // `runFiniTimeJobs()`, script.cpp:689: "Personality::onFini called as
        // last item here". A double drain only logs in the reference; the
        // port ignores the error and finalises regardless.
        let _ = init_time_job::run_fini_time_jobs();
        // `Script::clearTimers` (`py_callback.cpp:81-91`): the reference reaches it
        // from the app's shutdown, ahead of `Script::fini`; the port places it here,
        // behind the fini-time jobs, so a job that posts a callback is not the one
        // that leaves a callback behind for `Py_Finalize` to find. Nothing survives
        // the drain to the collect loop, which is what keeps a posted-but-never-run
        // callback from being a dangling one.
        gil::clear();
        // The native registries die before the script system finalises --
        // `CellApp::shutDown` deletes its entities before `Script::fini` --
        // so every native wrapper `Py_DecRef`s here, allocator still alive.
        wrapper::reset();
        // The Math module's providers release their held references next:
        // the teardown table must stay intact for the deallocs that release
        // fires, so the reset sits before `binding::reset`, which drops the
        // type objects themselves.
        math::reset();
        // The type registry's references go the same way: the static
        // `s_type_` objects outlive every script object, and the port holds
        // its registry references until here.
        binding::reset();
        // The keep-alive entries die with the native side too: `Pickler`'s
        // registry and the timer table are torn down inside
        // `Script::fini` before the collect loop (script.cpp:686), so a
        // held reference never survives into `Py_Finalize`.
        keepalive::reset();
        // The snapshot goes before the collect loop, the reference's DECREF
        // between `runFiniTimeJobs` and `while (PyGC_Collect() > 0);`.
        let snapshot = INIT_TIME_MODULES.swap(std::ptr::null_mut(), Ordering::SeqCst);
        // The writer's own teardown: buffers and hooks go now, the
        // reference's writer dies inside `Py_Finalize`'s sys clear. This
        // follows the jobs, so a callback's `print` still reaches hooks and
        // the sink, as it does in the reference.
        output_writer::reset();
        // SAFETY: the interpreter is up, this thread owns the GIL, and the
        // steps below run in the reference's order: the snapshot DECREF
        // lands between `runFiniTimeJobs` and the collect loop, while the
        // allocator is still alive -- freeing it after `Py_Finalize`
        // segfaults inside `_Py_Dealloc`.
        unsafe {
            // SAFETY: `dec_ref` skips null, so a double `fini` is a no-op.
            capi::dec_ref(snapshot);
            // `while (PyGC_Collect() > 0);`
            while capi::PyGC_Collect() > 0 {}
            clear_main_module();
            capi::Py_Finalize();
        }
        INITIALIZED.store(false, Ordering::SeqCst);
        capi::mark_finalized();
    }
}

impl Drop for Interpreter {
    fn drop(&mut self) {
        self.fini();
    }
}

/// `Py_DECREF`.
fn dec_ref(obj: *mut PyObject) {
    // SAFETY: every call site holds a pointer the C API gave out.
    unsafe {
        capi::dec_ref(obj);
    }
}

/// `PySys_SetObject("argv", pArgvList)`.
///
/// SAFETY: the interpreter must be up and the caller must own the GIL.
fn set_argv(argv: &[String]) -> bool {
    unsafe {
        let list = capi::PyList_New(argv.len() as capi::PySsize);
        if list.is_null() {
            return false;
        }
        // `PyList_SetItem` steals the item on success only, so a failure
        // DECREFs the item and then the list, exactly like the reference's
        // `Py_XDECREF(pArg); break;`.
        for (index, argument) in argv.iter().enumerate() {
            let Ok(encoded) = CString::new(argument.as_str()) else {
                dec_ref(list);
                return false;
            };
            let item = capi::PyUnicode_FromString(encoded.as_ptr());
            if item.is_null() || capi::PyList_SetItem(list, index as capi::PySsize, item) != 0 {
                capi::dec_ref(item);
                dec_ref(list);
                return false;
            }
        }
        let ok = capi::PySys_SetObject(c"argv".as_ptr(), list) == 0;
        dec_ref(list);
        ok
    }
}

/// `PySys_SetObject("path", paths.pathAsObject())`.
///
/// SAFETY: the interpreter must be up and the caller must own the GIL.
fn set_import_paths(paths: &ImportPaths) -> bool {
    // SAFETY: the interpreter is up and this thread owns the GIL; the handle
    // releases the list after the assignment, as the reference's
    // `Py_DECREF(pSys)` does.
    unsafe {
        let list = paths.path_as_object();
        if list.is_empty() {
            return false;
        }
        capi::PySys_SetObject(c"path".as_ptr(), list.as_ptr()) == 0
    }
}

/// `ScriptModule::getOrCreate("BigWorld")` plus the `component` attribute
/// (`script.cpp:589-600`). Both steps go through `ScriptErrorPrint` in the
/// reference, which logs and carries on, so a failure here neither aborts
/// init nor leaves the module half-made.
///
/// SAFETY: the interpreter must be up and the caller must own the GIL.
fn create_big_world_module(component: &str) {
    // SAFETY: the interpreter is up and this thread owns the GIL.
    unsafe {
        let module = capi::PyImport_AddModule(c"BigWorld".as_ptr());
        if module.is_null() {
            return;
        }
        let Ok(name) = std::ffi::CString::new(component) else {
            return;
        };
        let value = capi::PyUnicode_FromString(name.as_ptr());
        if value.is_null() {
            return;
        }
        // `setAttribute` failure is `ScriptErrorPrint()`ed away too; the
        // reference leaves the attribute unset and moves on.
        capi::PyObject_SetAttrString(module, c"component".as_ptr(), value);
        capi::dec_ref(value);
    }
}

/// `s_pOurInitTimeModules = PyDict_Copy(PySys_GetObject("modules"))`
/// (`script.cpp:602`).
///
/// SAFETY: the interpreter must be up and the caller must own the GIL.
fn take_modules_snapshot() {
    // SAFETY: the interpreter is up and this thread owns the GIL.
    unsafe {
        let modules = capi::PySys_GetObject(c"modules".as_ptr());
        let snapshot = capi::PyDict_Copy(modules);
        INIT_TIME_MODULES.store(snapshot, Ordering::SeqCst);
    }
}

/// `Script::disablePythonGarbage`: import `gc` and call `gc.disable()`.
///
/// The reference clears an import failure and carries on with collection
/// enabled; the port reports it instead, see [`InitError`].
///
/// SAFETY: the interpreter must be up and the caller must own the GIL.
/// `ScriptModule::import("bw_site")`: the engine's site-module stand-in,
/// imported from the res tree at the tail of init. A failure leaves the
/// pending exception for the caller's rollback path to clear.
fn import_bw_site() -> bool {
    // SAFETY: the interpreter is up and this thread owns the GIL.
    unsafe {
        let module = capi::PyImport_ImportModule(c"bw_site".as_ptr());
        if module.is_null() {
            capi::clear_error();
            return false;
        }
        capi::dec_ref(module);
        true
    }
}

fn disable_garbage() -> bool {
    unsafe {
        let module = capi::PyImport_ImportModule(c"gc".as_ptr());
        if module.is_null() {
            capi::PyErr_Clear();
            return false;
        }
        let disabled = call_gc_disable(module);
        capi::dec_ref(module);
        disabled
    }
}

/// `pGCModule.callMethod("disable")` through `PyObject_GetAttrString` +
/// `PyObject_CallNoArgs`: true when the call went through.
///
/// SAFETY: `module` must be a borrowed module object and the caller must own
/// the GIL.
fn call_gc_disable(module: *mut PyObject) -> bool {
    unsafe {
        let method = capi::PyObject_GetAttrString(module, c"disable".as_ptr());
        if method.is_null() {
            capi::PyErr_Clear();
            return false;
        }
        let result = capi::PyObject_CallNoArgs(method);
        capi::dec_ref(method);
        if result.is_null() {
            capi::PyErr_Clear();
            return false;
        }
        capi::dec_ref(result);
        true
    }
}

/// The `__main__` cleanup from `Script::fini`.
///
/// SAFETY: the interpreter must be up and the caller must own the GIL.
fn clear_main_module() {
    unsafe {
        let modules = capi::PyImport_GetModuleDict();
        if modules.is_null() {
            return;
        }
        let main = capi::PyDict_GetItemString(modules, c"__main__".as_ptr());
        if main.is_null() || !capi::py_module_check(main) {
            return;
        }
        let dictionary = capi::PyModule_GetDict(main);
        if dictionary.is_null() {
            return;
        }
        let mut position: capi::PySsize = 0;
        let mut key: *mut PyObject = std::ptr::null_mut();
        let mut value: *mut PyObject = std::ptr::null_mut();
        while capi::PyDict_Next(dictionary, &mut position, &mut key, &mut value) != 0 {
            // `PyUnicode_Check(key) && strcmp(PyUnicode_AsUTF8(key),
            // "__builtins__") == 0` folds into one comparison call: a
            // non-string key compares unequal, exactly like the reference's
            // guard.
            if capi::PyUnicode_CompareWithASCIIString(key, c"__builtins__".as_ptr()) == 0 {
                continue;
            }
            capi::PyDict_SetItem(dictionary, key, capi::py_none());
        }
        capi::PyDict_SetItemString(modules, c"__main__".as_ptr(), capi::py_none());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gil;
    use crate::import_paths;
    use crate::keepalive;
    use crate::manifest;
    use crate::pyref::PyRef;
    use crate::sequence;
    use std::path::PathBuf;
    use std::process::Command;

    /// One container state plus the view over it: the sequence scenarios
    /// build a proxy on it and park the view in `__main__`, the same way the
    /// engine parks a native object.
    fn sequence_view(values: &[i64], writable: bool) -> (Box<sequence::SequenceState>, PyRef) {
        let mut state = if writable {
            Box::new(sequence::SequenceState::new(values))
        } else {
            Box::new(sequence::SequenceState::read_only(values))
        };
        let view = unsafe { sequence::wrap(state.as_mut() as *mut sequence::SequenceState) }
            .expect("wrap the native state");
        (state, view)
    }

    /// BW-03-01-01. One scenario per process, because CPython 3.13 and later
    /// refuse to initialise a process that has already finalised (`Fatal
    /// Python error: _PyImport_Init: global import state already
    /// initialized`) and the reference never re-initialises either:
    /// `Script::createInterpreter` swaps subinterpreters and `Script::fini`
    /// runs once at process end. Each scenario is therefore an ignored test,
    /// and the parent runs them out-of-process.
    const SCENARIOS: &[&str] = &[
        "interpreter::tests::scenario_pre_init_flags",
        "interpreter::tests::scenario_environment_is_ignored",
        "interpreter::tests::scenario_argv_is_assigned_when_supplied",
        "interpreter::tests::scenario_argv_is_left_alone_when_empty",
        "interpreter::tests::scenario_garbage_collection_is_disabled",
        "interpreter::tests::scenario_init_is_idempotent_while_up",
        "interpreter::tests::scenario_fini_finalises_once",
        "interpreter::tests::scenario_fini_blanks_main_module",
        "interpreter::tests::scenario_drop_runs_fini",
        "interpreter::tests::scenario_a_failed_source_reports_failure",
        "interpreter::tests::scenario_a_bad_argument_is_an_init_failure",
        "interpreter::tests::scenario_a_handle_owns_one_reference",
        "interpreter::tests::scenario_a_handle_outliving_finalisation_is_safe",
        "interpreter::tests::scenario_a_script_under_the_resource_root_imports",
        "interpreter::tests::scenario_sys_path_is_the_assembled_list",
        "interpreter::tests::scenario_an_unencodable_path_fails_init",
        "interpreter::tests::scenario_big_world_module_carries_the_component",
        "interpreter::tests::scenario_a_component_with_a_nul_stays_unset",
        "interpreter::tests::scenario_stdout_lands_in_the_log_sink",
        "interpreter::tests::scenario_stderr_lands_in_the_log_sink",
        "interpreter::tests::scenario_partial_writes_buffer_and_hooks_see_chunks",
        "interpreter::tests::scenario_a_hook_may_remove_itself_mid_callback",
        "interpreter::tests::scenario_an_embedded_nul_logs_the_repr",
        "interpreter::tests::scenario_the_excepthook_formats_the_traceback",
        "interpreter::tests::scenario_the_excepthook_handles_a_missing_traceback",
        "interpreter::tests::scenario_bad_write_arguments_raise",
        "interpreter::tests::scenario_bw_site_is_imported_at_the_tail_of_init",
        "interpreter::tests::scenario_missing_bw_site_fails_init",
        "interpreter::tests::scenario_init_time_modules_snapshot_taken_at_init",
        "interpreter::tests::scenario_init_time_jobs_run_once_in_rung_order",
        "interpreter::tests::scenario_fini_time_jobs_run_once_in_rung_order",
        "interpreter::tests::scenario_fini_runs_the_fini_time_jobs",
        "interpreter::tests::scenario_the_same_native_id_wraps_to_one_object",
        "interpreter::tests::scenario_a_forgotten_id_wraps_to_a_new_object",
        "interpreter::tests::scenario_a_foreign_object_carries_no_native_id",
        "interpreter::tests::scenario_different_ids_are_distinct_wrappers",
        "interpreter::tests::scenario_the_cache_holds_one_reference_per_identity",
        "interpreter::tests::scenario_a_bound_attribute_round_trips",
        "interpreter::tests::scenario_a_bound_type_refuses_the_wrong_access",
        "interpreter::tests::scenario_a_bound_method_parses_its_arguments",
        "interpreter::tests::scenario_a_script_cannot_construct_a_native_type",
        "interpreter::tests::scenario_an_instance_renders_with_the_reference_format",
        "interpreter::tests::scenario_a_type_name_creates_one_type_object",
        "interpreter::tests::scenario_a_sequence_proxy_round_trips_native_state",
        "interpreter::tests::scenario_two_sequence_views_share_native_state",
        "interpreter::tests::scenario_a_sequence_proxy_normalises_negative_indices",
        "interpreter::tests::scenario_a_sequence_proxy_bounds_are_checked",
        "interpreter::tests::scenario_a_read_only_sequence_refuses_assignment",
        "interpreter::tests::scenario_a_non_integer_subscript_refuses",
        "interpreter::tests::scenario_a_keep_alive_entry_holds_one_reference",
        "interpreter::tests::scenario_repinning_adopts_and_replaces_once",
        "interpreter::tests::scenario_unpinning_twice_releases_once",
        "interpreter::tests::scenario_a_pinned_callback_survives_the_script_dropping_it",
        "interpreter::tests::scenario_a_script_side_cycle_is_freed_exactly_once",
        "interpreter::tests::scenario_the_gil_context_is_installed_at_init",
        "interpreter::tests::scenario_a_callback_posted_from_a_dying_thread_runs_on_the_script_thread",
        "interpreter::tests::scenario_an_exception_in_a_callback_does_not_leave_it_queued",
        "interpreter::tests::scenario_finishing_the_script_system_leaves_no_callback_behind",
        "interpreter::tests::scenario_init_runs_the_init_time_jobs",
        "interpreter::tests::scenario_init_run_fini",
        "interpreter::tests::scenario_the_manifest_matches_the_interpreter",
        "interpreter::tests::scenario_the_math_module_serves_the_ported_surface",
        "interpreter::tests::scenario_the_vector_types_exercise_their_surface",
        "interpreter::tests::scenario_the_matrix_and_plane_types_exercise_their_surface",
        "interpreter::tests::scenario_the_matrix_and_plane_methods_exercise_their_surface",
        "interpreter::tests::scenario_the_v4_providers_exercise_their_surface",
    ];

    /// Runs `scenario` in a fresh process of this test binary and requires it to
    /// exit normally, surfacing its output when it does not.
    fn run_scenario(scenario: &str) {
        run_scenario_with_env(scenario, &[])
    }

    /// [`run_scenario`] with extra environment variables applied to the child.
    fn run_scenario_with_env(scenario: &str, extra_env: &[(&str, &str)]) {
        let harness = std::env::current_exe().expect("the test binary path");
        let mut command = Command::new(&harness);
        command.args(["--exact", scenario, "--ignored", "--test-threads=1"]);
        for (key, value) in extra_env {
            command.env(key, value);
        }
        let output = command.output().expect("spawn the scenario child");
        assert!(
            output.status.success(),
            "scenario {scenario} did not exit normally: {}\n--- stdout ---\n{}\n--- stderr ---\n{}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
    }

    /// `Script::init` around one body, then `Script::fini`.
    fn interpreter_scenario(config: &InitConfig, body: impl FnOnce(&Interpreter)) {
        let interpreter = Interpreter::init(&scenario_config(config)).expect("Script::init");
        body(&interpreter);
        interpreter.fini();
    }

    /// `Interpreter::init`'s config with the scenario fixture's res root in
    /// the path list, so `import bw_site` has something to import: the
    /// reference always has the engine's script tree on its path, and the
    /// fixture stands in for it.
    fn scenario_config(config: &InitConfig) -> InitConfig {
        let root = temp_resource_root();
        let root_path = root.to_str().expect("a UTF-8 root").to_owned();
        let mut paths = config.import_paths.clone();
        paths.add_res_path(&[&root_path], import_paths::COMMON_PATH);
        InitConfig {
            import_paths: paths,
            ..config.clone()
        }
    }

    /// A throw-away resource root laid out the way the engine ships its scripts:
    /// `scripts/common` holds one importable module.
    fn temp_resource_root() -> PathBuf {
        let root =
            std::env::temp_dir().join(format!("boundless-python-host-{}", std::process::id()));
        std::fs::remove_dir_all(&root).ok();
        std::fs::create_dir_all(root.join("scripts/common/Lib")).expect("build the script tree");
        std::fs::write(root.join("scripts/common/probe_script.py"), "VALUE = 7\n")
            .expect("write the script");
        std::fs::write(
            root.join("scripts/common/bw_site.py"),
            "import sys\nsys.bw_site_imported = True\n",
        )
        .expect("write bw_site");
        std::fs::write(
            root.join("scripts/common/boom.py"),
            "def boom():\n    raise ValueError('boom message')\n",
        )
        .expect("write the boom script");
        root
    }

    /// Runs `source` from inside an init-time job and reports whether the
    /// `__probe` it assigns renders true.
    fn run_simple_probe(source: &str) -> bool {
        // SAFETY: only called from an init-time job, which runs while the
        // interpreter is up on the thread that owns the GIL.
        unsafe {
            let Ok(encoded) = std::ffi::CString::new(source) else {
                return false;
            };
            if capi::PyRun_SimpleString(encoded.as_ptr()) != 0 {
                // The failed run leaves a pending exception; init continues
                // after an init-time job either way, and a pending exception
                // would poison the next call, so the helper prints-and-clears
                // the way the reference's `ScriptErrorPrint` does.
                capi::clear_error();
                return false;
            }
            let main = capi::PyImport_AddModule(c"__main__".as_ptr());
            let dictionary = capi::PyModule_GetDict(main);
            let value = capi::PyDict_GetItemString(dictionary, c"__probe".as_ptr());
            if value.is_null() {
                return false;
            }
            let text = capi::PyObject_Str(value);
            let rendered = capi::to_string(text);
            capi::dec_ref(text);
            rendered == "True"
        }
    }

    /// `sys.getrefcount(__handle)`: the live reference count of the object the
    /// handle scenario parked in `__main__`.
    fn refcount(interpreter: &Interpreter) -> i64 {
        probe(
            interpreter,
            "import sys; __probe = sys.getrefcount(__handle)",
        )
        .parse()
        .expect("a reference count")
    }

    /// Runs `source` and reads the `__probe` it assigns back as a string.
    fn probe(interpreter: &Interpreter, source: &str) -> String {
        assert!(
            interpreter.run_simple_string(source),
            "script must run: {source}"
        );
        unsafe {
            let main = capi::PyImport_AddModule(c"__main__".as_ptr());
            let dictionary = capi::PyModule_GetDict(main);
            let value = capi::PyDict_GetItemString(dictionary, c"__probe".as_ptr());
            let text = capi::PyObject_Str(value);
            let rendered = capi::to_string(text);
            capi::dec_ref(text);
            rendered
        }
    }

    /// A fresh list parked in `__main__` under `__handle`, as a borrowed handle
    /// plus a copy of it that names the object the same way the script does.
    /// The `refcount` helper reads `__handle`, so the parked name is what
    /// keeps the object readable between handles.
    fn fresh() -> PyRef {
        // SAFETY: the interpreter is up and this thread owns the GIL.
        unsafe {
            let list = PyRef::from_owned(capi::PyList_New(0)).expect("a live object");
            let main = capi::PyImport_AddModule(c"__main__".as_ptr());
            let dictionary = capi::PyModule_GetDict(main);
            assert_eq!(
                capi::PyDict_SetItemString(dictionary, c"__handle".as_ptr(), list.as_ptr()),
                0
            );
            list.clone()
        }
    }

    /// CPython starts with the three legacy pre-init flags off; init turns
    /// them all on, the way `Script::init` writes them. `sys.flags` exposes
    /// two of the three -- `Py_FrozenFlag` has no `sys.flags` field (it is
    /// Android-only in the C API) -- so the globals are read back where init
    /// writes them.
    #[test]
    #[ignore = "runs in its own process: see SCENARIOS"]
    fn scenario_pre_init_flags() {
        interpreter_scenario(&InitConfig::default(), |interpreter| {
            let flags = probe(
                interpreter,
                "import sys; __probe = (sys.flags.no_site, \
                 sys.flags.ignore_environment)",
            );
            assert_eq!(flags, "(1, 1)");
            assert_eq!(capi::pre_init_flags(), (1, 1, 1));
        });
    }

    /// `Py_IgnoreEnvironmentFlag = 1`: PYTHONPATH does not reach `sys.path`.
    #[test]
    #[ignore = "runs in its own process: see SCENARIOS"]
    fn scenario_environment_is_ignored() {
        interpreter_scenario(&InitConfig::default(), |interpreter| {
            let ignored = probe(
                interpreter,
                "import sys; __probe = '/no/such/python/path' \
                 not in ''.join(sys.path)",
            );
            assert_eq!(ignored, "True");
        });
    }

    /// `PySys_SetObject("argv", pArgvList)` with the reference's exact list
    /// content.
    #[test]
    #[ignore = "runs in its own process: see SCENARIOS"]
    fn scenario_argv_is_assigned_when_supplied() {
        interpreter_scenario(
            &InitConfig {
                argv: vec![String::from("baseapp"), String::from("--res")],
                ..Default::default()
            },
            |interpreter| {
                assert_eq!(
                    probe(interpreter, "import sys; __probe = sys.argv"),
                    "['baseapp', '--res']"
                );
            },
        );
    }

    /// An empty argv list leaves `sys.argv` untouched, the reference's
    /// `if (g_scriptArgc)` guard.
    #[test]
    #[ignore = "runs in its own process: see SCENARIOS"]
    fn scenario_argv_is_left_alone_when_empty() {
        interpreter_scenario(&InitConfig::default(), |interpreter| {
            assert_eq!(
                probe(
                    interpreter,
                    "import sys; __probe = sys.argv != ['baseapp', '--res']"
                ),
                "True"
            );
        });
    }

    /// `Script::disablePythonGarbage` calls `gc.disable()`.
    #[test]
    #[ignore = "runs in its own process: see SCENARIOS"]
    fn scenario_garbage_collection_is_disabled() {
        interpreter_scenario(&InitConfig::default(), |interpreter| {
            assert_eq!(
                probe(interpreter, "import gc; __probe = gc.isenabled()"),
                "False"
            );
        });
    }

    /// `if (s_isInitalised) return true;` plus the two reference flags.
    #[test]
    #[ignore = "runs in its own process: see SCENARIOS"]
    fn scenario_init_is_idempotent_while_up() {
        interpreter_scenario(&InitConfig::default(), |_interpreter| {
            let second = Interpreter::init(&InitConfig::default());
            assert!(second.is_ok());
            let third = Interpreter::init(&InitConfig::default());
            assert!(third.is_ok());
            assert!(Interpreter::is_initialized());
            assert!(!Interpreter::is_finalized());
        });
    }

    /// `Script::fini` marks the script system finalised, clears the flag, and
    /// a second `fini` is a no-op rather than a second `Py_Finalize`.
    #[test]
    #[ignore = "runs in its own process: see SCENARIOS"]
    fn scenario_fini_finalises_once() {
        let interpreter =
            Interpreter::init(&scenario_config(&InitConfig::default())).expect("Script::init");
        interpreter.fini();
        interpreter.fini();
        assert!(Interpreter::is_finalized());
        assert!(!Interpreter::is_initialized());
        assert_eq!(unsafe { capi::Py_IsInitialized() }, 0);
    }

    /// The `_PyModule_Clear` re-implementation: the `__main__` entry in
    /// `sys.modules` becomes `None` (and the module dict loses its
    /// attributes) before finalise.
    #[test]
    #[ignore = "runs in its own process: see SCENARIOS"]
    fn scenario_fini_blanks_main_module() {
        interpreter_scenario(&InitConfig::default(), |interpreter| {
            interpreter.run_simple_string("value = 42");
            clear_main_module();
            // SAFETY: the interpreter is up and this thread owns the GIL.
            unsafe {
                let modules = capi::PyImport_GetModuleDict();
                let entry = capi::PyDict_GetItemString(modules, c"__main__".as_ptr());
                assert_eq!(entry, capi::py_none());
            }
        });
    }

    /// Dropping the last handle runs `Script::fini`, so a role runtime does
    /// not have to remember the teardown.
    #[test]
    #[ignore = "runs in its own process: see SCENARIOS"]
    fn scenario_drop_runs_fini() {
        {
            let _interpreter =
                Interpreter::init(&scenario_config(&InitConfig::default())).expect("Script::init");
        }
        assert!(Interpreter::is_finalized());
    }

    /// Real stdlib work through the embedded interpreter, the shape of a
    /// server process start.
    ///
    /// The reference replaces `sys.path` rather than extending it, so only
    /// frozen and builtin modules stay importable: the probe imports `sys` and
    /// `gc`, both builtin, plus a script from the resource tree.
    #[test]
    #[ignore = "runs in its own process: see SCENARIOS"]
    fn scenario_init_run_fini() {
        let root = temp_resource_root();
        let root_path = root.to_str().expect("a UTF-8 root");
        let mut paths = ImportPaths::new(ImportPaths::default_delimiter());
        paths.add_res_path(&[root_path], "scripts/common");
        let interpreter = Interpreter::init(&InitConfig {
            argv: vec![String::from("scenario")],
            import_paths: paths,
            ..Default::default()
        })
        .expect("Script::init");
        // Real stdlib modules reach the embedded interpreter, and a script from
        // the resource tree resolves through the assembled `sys.path`.
        let flags = probe(
            &interpreter,
            "import sys, gc, probe_script\n\
             __probe = (sys.argv[0], gc.isenabled(), probe_script.VALUE)",
        );
        assert_eq!(flags, "('scenario', False, 7)");
        interpreter.fini();
        std::fs::remove_dir_all(&root).ok();
    }

    /// BW-03-01-01 acceptance: every scenario exits normally.
    #[test]
    fn every_scenario_exits_cleanly() {
        for scenario in SCENARIOS {
            run_scenario(scenario);
        }
    }

    /// The environment flag only proves anything with the variable set.
    #[test]
    fn python_path_is_ignored_when_the_flag_is_set() {
        run_scenario_with_env(
            "interpreter::tests::scenario_environment_is_ignored",
            &[("PYTHONPATH", "/no/such/python/path")],
        );
    }

    /// BW-03-01-01 acceptance: repeated runs of an independent process all
    /// exit normally.
    #[test]
    fn repeated_independent_processes_exit_cleanly() {
        for _ in 0..5 {
            run_scenario("interpreter::tests::scenario_init_run_fini");
        }
    }

    /// A `PyRun_SimpleString` failure reaches the host as `false`, and a
    /// source with an embedded NUL cannot be handed to the C API at all:
    /// both come back as a failed run, never a panic.
    #[test]
    #[ignore = "runs in its own process: see SCENARIOS"]
    fn scenario_a_failed_source_reports_failure() {
        interpreter_scenario(&InitConfig::default(), |interpreter| {
            assert!(!interpreter.run_simple_string("this is not python at all"));
            assert!(!interpreter.run_simple_string("print('embedded\0nul')"));
            // A failed run must not have taken the interpreter down.
            assert!(Interpreter::is_initialized());
            clear_main_module();
            clear_main_module();
        });
    }

    /// An argv element with an embedded NUL cannot become a C string, so
    /// `Script::init` unwinds back to the pre-init state and reports
    /// `ArgvFailed` instead of half-initialising the interpreter.
    #[test]
    #[ignore = "runs in its own process: see SCENARIOS"]
    fn scenario_a_bad_argument_is_an_init_failure() {
        assert_eq!(
            Interpreter::init(&InitConfig {
                argv: vec![String::from("bad\0argument")],
                ..Default::default()
            })
            .err(),
            Some(InitError::ArgvFailed),
        );
        assert!(!Interpreter::is_initialized());
        // The interpreter was torn down again, so nothing is left behind.
        assert_eq!(unsafe { capi::Py_IsInitialized() }, 0);
    }

    /// `PyRef` owns exactly one reference: a clone increments it, dropping the
    /// clone decrements it back, and a clone names the same object. The
    /// counts are read back through `sys.getrefcount`, so a derived `Clone`
    /// that forgot to increment fails the assertion instead of freeing the
    /// object early.
    #[test]
    #[ignore = "runs in its own process: see SCENARIOS"]
    fn scenario_a_handle_owns_one_reference() {
        interpreter_scenario(&InitConfig::default(), |interpreter| {
            // SAFETY: the interpreter is up and this thread owns the GIL.
            let list = unsafe { PyRef::from_owned(capi::PyList_New(1)) }.expect("a live object");
            assert!(!list.is_empty());
            // A second owner in `__main__` keeps the object readable after the
            // handles go away.
            unsafe {
                let main = capi::PyImport_AddModule(c"__main__".as_ptr());
                let dictionary = capi::PyModule_GetDict(main);
                assert_eq!(
                    capi::PyDict_SetItemString(dictionary, c"__handle".as_ptr(), list.as_ptr(),),
                    0,
                );
            }
            let before = refcount(interpreter);
            let cloned = list.clone();
            assert_eq!(list, cloned);
            assert_eq!(list.as_ptr(), cloned.as_ptr());
            let with_clone = refcount(interpreter);
            drop(cloned);
            let without_clone = refcount(interpreter);
            assert_eq!(with_clone, before + 1);
            assert_eq!(without_clone, before);
            drop(list);
        });
    }

    /// A handle outliving `Script::fini` must not decrement into a torn-down
    /// interpreter, so `PyRef::drop` skips it once the script system is
    /// finalised. The object is a fresh list with no other owner: an actual
    /// decrement after finalise would hit the torn-down allocator and this
    /// process would not exit normally.
    #[test]
    #[ignore = "runs in its own process: see SCENARIOS"]
    fn scenario_a_handle_outliving_finalisation_is_safe() {
        let interpreter =
            Interpreter::init(&scenario_config(&InitConfig::default())).expect("Script::init");
        // SAFETY: the interpreter is up and this thread owns the GIL.
        let late = unsafe { PyRef::from_owned(capi::PyList_New(0)) }.expect("a live object");
        interpreter.fini();
        assert!(Interpreter::is_finalized());
        // The drop below runs after finalisation: it must be a no-op.
        drop(late);
    }

    /// BW-03-01-02 acceptance: a script that lives where the engine ships its
    /// scripts imports by name, because `sys.path` is the assembled resource
    /// path list rather than CPython's default.
    #[test]
    #[ignore = "runs in its own process: see SCENARIOS"]
    fn scenario_a_script_under_the_resource_root_imports() {
        let root = temp_resource_root();
        let root_path = root.to_str().expect("a UTF-8 root");
        let mut app = ImportPaths::new(ImportPaths::default_delimiter());
        app.add_non_res_path(root.join("app/scripts").to_str().expect("a path"));
        let sys_paths =
            import_paths::role_paths(&app, &[root_path], "linux-x86-64", Some("server"));
        let interpreter = Interpreter::init(&InitConfig {
            argv: vec![],
            import_paths: sys_paths,
            ..Default::default()
        })
        .expect("Script::init");
        assert_eq!(
            probe(
                &interpreter,
                "import probe_script; __probe = probe_script.VALUE"
            ),
            "7"
        );
        interpreter.fini();
        std::fs::remove_dir_all(&root).ok();
    }

    /// `sys.path` is the app's paths followed by the resource-tree paths, in
    /// the reference's order: `common`, `common/Lib`, then the platform
    /// build directory.
    #[test]
    #[ignore = "runs in its own process: see SCENARIOS"]
    fn scenario_sys_path_is_the_assembled_list() {
        let mut app = ImportPaths::new(ImportPaths::default_delimiter());
        app.add_non_res_path("/app/scripts");
        let sys_paths = import_paths::role_paths(&app, &["/res"], "linux-x86-64", Some("server"));
        interpreter_scenario(
            &InitConfig {
                argv: vec![],
                import_paths: sys_paths,
                ..Default::default()
            },
            |interpreter| {
                // The fixture root's `scripts/common` trails the assembled
                // list, standing in for the engine's own script tree being
                // on the path.
                assert_eq!(
                    probe(interpreter, "import sys; __probe = len(sys.path)"),
                    "5"
                );
                assert_eq!(
                    probe(interpreter, "import sys; __probe = sys.path[3]"),
                    "/res/scripts/server_common/lib-dynload-linux-x86-64_server"
                );
            },
        );
    }

    /// A path with an embedded NUL cannot become a C string. The collection
    /// refuses to build the list rather than silently dropping the entry, so
    /// init fails and unwinds, the same way a bad argument does.
    #[test]
    #[ignore = "runs in its own process: see SCENARIOS"]
    fn scenario_an_unencodable_path_fails_init() {
        let mut paths = ImportPaths::new(ImportPaths::default_delimiter());
        paths.add_non_res_path("/res\0/scripts");
        assert_eq!(
            Interpreter::init(&InitConfig {
                argv: vec![],
                import_paths: paths,
                ..Default::default()
            })
            .err(),
            Some(InitError::PathFailed),
        );
        assert!(!Interpreter::is_initialized());
        assert_eq!(unsafe { capi::Py_IsInitialized() }, 0);
    }

    /// The `BigWorld` module `Script::init` creates and the `component`
    /// attribute every script reads (`script.cpp:589-600`).
    #[test]
    #[ignore = "runs in its own process: see SCENARIOS"]
    fn scenario_big_world_module_carries_the_component() {
        interpreter_scenario(
            &InitConfig {
                component: String::from("cell"),
                ..Default::default()
            },
            |interpreter| {
                assert_eq!(
                    probe(interpreter, "import BigWorld; __probe = BigWorld.component"),
                    "cell"
                );
                assert_eq!(
                    probe(
                        interpreter,
                        "import sys; __probe = 'BigWorld' in sys.modules"
                    ),
                    "True"
                );
            },
        );
    }

    /// A component name with an embedded NUL cannot be encoded for
    /// `setAttribute`, so the attribute is left unset -- the reference's
    /// `ScriptErrorPrint` failure path for the `BigWorld` module and its
    /// attribute, both of which leave init moving.
    #[test]
    #[ignore = "runs in its own process: see SCENARIOS"]
    fn scenario_a_component_with_a_nul_stays_unset() {
        interpreter_scenario(
            &InitConfig {
                component: String::from("bad\u{0}name"),
                ..Default::default()
            },
            |interpreter| {
                assert_eq!(
                    probe(
                        interpreter,
                        "import BigWorld; __probe = hasattr(BigWorld, 'component')"
                    ),
                    "False"
                );
            },
        );
    }

    /// `s_pOurInitTimeModules`: the module table as it stood at the end of
    /// init -- `BigWorld` in, anything imported later out. `Script::fini`
    /// releases it, so the accessor reads null again afterwards.
    #[test]
    #[ignore = "runs in its own process: see SCENARIOS"]
    fn scenario_init_time_modules_snapshot_taken_at_init() {
        let root = temp_resource_root();
        let root_path = root.to_str().expect("a UTF-8 root");
        let mut paths = ImportPaths::new(ImportPaths::default_delimiter());
        paths.add_res_path(&[root_path], "scripts/common");
        let interpreter = Interpreter::init(&InitConfig {
            component: String::from("base"),
            import_paths: paths,
            ..Default::default()
        })
        .expect("Script::init");
        assert_eq!(
            probe(
                &interpreter,
                "import probe_script; __probe = probe_script.VALUE"
            ),
            "7"
        );
        // SAFETY: the interpreter is up and this thread owns the GIL.
        unsafe {
            let snapshot = Interpreter::init_time_modules();
            assert!(!snapshot.is_null());
            assert!(capi::PyDict_GetItemString(snapshot, c"probe_script".as_ptr()).is_null());
            assert!(!capi::PyDict_GetItemString(snapshot, c"BigWorld".as_ptr()).is_null());
        }
        interpreter.fini();
        assert!(Interpreter::init_time_modules().is_null());
        std::fs::remove_dir_all(&root).ok();
    }

    /// Init-time jobs run once, ascending rung, insertion order within a
    /// rung; a second run and a registration after the run are both
    /// `AlreadyRan` (the reference's error and critical error).
    #[test]
    #[ignore = "runs in its own process: see SCENARIOS"]
    fn scenario_init_time_jobs_run_once_in_rung_order() {
        let order = std::sync::Arc::new(std::sync::Mutex::new(Vec::<i32>::new()));
        for rung in [10, -5, 0, 0] {
            let order = order.clone();
            assert_eq!(
                init_time_job::add_init_time_job(rung, move || order.lock().unwrap().push(rung)),
                Ok(())
            );
        }
        assert_eq!(init_time_job::run_init_time_jobs(), Ok(()));
        assert_eq!(*order.lock().unwrap(), vec![-5, 0, 0, 10]);
        assert_eq!(
            init_time_job::run_init_time_jobs(),
            Err(init_time_job::JobError::AlreadyRan)
        );
        assert_eq!(
            init_time_job::add_init_time_job(0, || {}),
            Err(init_time_job::JobError::AlreadyRan)
        );
    }

    /// Fini-time jobs share the contract; `add_fini_time_job` is the
    /// reference's default rung 1.
    #[test]
    #[ignore = "runs in its own process: see SCENARIOS"]
    fn scenario_fini_time_jobs_run_once_in_rung_order() {
        let order = std::sync::Arc::new(std::sync::Mutex::new(Vec::<i32>::new()));
        {
            let order = order.clone();
            assert_eq!(
                init_time_job::add_fini_time_job(move || order.lock().unwrap().push(1)),
                Ok(())
            );
        }
        for rung in [3, -2] {
            let order = order.clone();
            assert_eq!(
                init_time_job::add_fini_time_job_at(rung, move || order.lock().unwrap().push(rung)),
                Ok(())
            );
        }
        assert_eq!(init_time_job::run_fini_time_jobs(), Ok(()));
        assert_eq!(*order.lock().unwrap(), vec![-2, 1, 3]);
        assert_eq!(
            init_time_job::run_fini_time_jobs(),
            Err(init_time_job::JobError::AlreadyRan)
        );
        assert_eq!(
            init_time_job::add_fini_time_job(|| {}),
            Err(init_time_job::JobError::AlreadyRan)
        );
    }

    /// `Script::fini` drains the fini-time jobs itself, before any
    /// teardown: every callback still sees a live interpreter, its script
    /// calls still reach CPython, and its output still lands in the log
    /// sink. The reference runs `runFiniTimeJobs()` at script.cpp:689 --
    /// "`Personality::onFini` called as last item here" -- before the
    /// snapshot DECREF and the collect loop.
    #[test]
    #[ignore = "runs in its own process: see SCENARIOS"]
    fn scenario_fini_runs_the_fini_time_jobs() {
        let events = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        {
            let events = events.clone();
            output_writer::set_log_sink(move |line, _is_stderr| {
                events.lock().unwrap().push(format!("sink:{line}"));
            });
        }
        interpreter_scenario(&InitConfig::default(), |_interpreter| {
            let rung_zero = events.clone();
            assert_eq!(
                init_time_job::add_fini_time_job_at(0, move || {
                    // The interpreter is still up: the callback runs script
                    // and its print reaches the sink.
                    assert!(run_simple_probe("print('from fini job'); __probe = True"));
                    rung_zero
                        .lock()
                        .unwrap()
                        .push(format!("job:alive={}", Interpreter::is_initialized()));
                }),
                Ok(())
            );
            let rung_two = events.clone();
            assert_eq!(
                init_time_job::add_fini_time_job_at(2, move || {
                    rung_two.lock().unwrap().push("job:onFini-rung".to_owned());
                }),
                Ok(())
            );
        });
        // The teardown events land in order; what follows is the
        // documented exit-time flush noise of the reference's own writer.
        let recorded = events.lock().unwrap().clone();
        assert!(
            recorded.starts_with(&[
                "sink:from fini job".to_owned(),
                "job:alive=true".to_owned(),
                "job:onFini-rung".to_owned()
            ]),
            "{recorded:?}"
        );
        assert!(
            recorded[3..]
                .iter()
                .all(|line| line.contains("flush") || line.contains("unraisablehook")),
            "{recorded:?}"
        );
        assert!(Interpreter::is_finalized());
    }

    /// The reference identity rule, one scenario per facet of it: the same
    /// native id always yields the same wrapper instance (`PyEntities::
    /// subscript` hands back the one embedded `PyObjectPlus` with
    /// `Py_XINCREF`), a forgotten id rewraps to a new object, a foreign
    /// object carries no native id, and the cache holds exactly one
    /// reference per registered identity.
    #[test]
    #[ignore = "runs in its own process: see SCENARIOS"]
    fn scenario_the_same_native_id_wraps_to_one_object() {
        interpreter_scenario(&InitConfig::default(), |interpreter| {
            let native = 0x1000 as *mut std::os::raw::c_void;
            let first = unsafe { wrapper::wrap(native, 100) }.expect("wrap the native object");
            let second = unsafe { wrapper::wrap(native, 100) }.expect("wrap it again");
            // Same pointer, so Python sees `is` identity: the subscript
            // layer's `Py_XINCREF` and return.
            assert_eq!(first, second);
            assert_eq!(unsafe { wrapper::native(&first) }, Some(native));
            // The capsule carries the wrapper name.
            let main = unsafe { capi::PyImport_AddModule(c"__main__".as_ptr()) };
            unsafe {
                capi::PyObject_SetAttrString(main, c"one".as_ptr(), first.as_ptr());
                capi::PyObject_SetAttrString(main, c"two".as_ptr(), second.as_ptr());
            }
            // The reference's `is` check, on the Python side.
            assert_eq!(probe(interpreter, "__probe = (one is two)"), "True");
        });
    }

    /// A forgotten id wraps again -- the reference has no second wrapper to
    /// hand back, and neither does the port.
    #[test]
    #[ignore = "runs in its own process: see SCENARIOS"]
    fn scenario_a_forgotten_id_wraps_to_a_new_object() {
        interpreter_scenario(&InitConfig::default(), |interpreter| {
            let native = 0x2000 as *mut std::os::raw::c_void;
            let first = unsafe { wrapper::wrap(native, 200) }.expect("wrap");
            let first_address = first.as_ptr() as usize;
            assert!(wrapper::release(200));
            let second = unsafe { wrapper::wrap(native, 200) }.expect("rewrap");
            assert_ne!(second.as_ptr() as usize, first_address);
            // The first wrapper is still alive while Python holds it: the
            // host dropping the identity only forgets the cache entry.
            let main = unsafe { capi::PyImport_AddModule(c"__main__".as_ptr()) };
            unsafe {
                capi::PyObject_SetAttrString(main, c"kept".as_ptr(), first.as_ptr());
                capi::PyObject_SetAttrString(main, c"fresh".as_ptr(), second.as_ptr());
            }
            assert_eq!(probe(interpreter, "__probe = (kept is fresh)"), "False");
        });
    }

    /// A foreign object (a plain `str`) is not one of this module's
    /// capsules: `PyCapsule_IsValid` refuses it.
    #[test]
    #[ignore = "runs in its own process: see SCENARIOS"]
    fn scenario_a_foreign_object_carries_no_native_id() {
        interpreter_scenario(&InitConfig::default(), |_interpreter| {
            let text = unsafe { capi::PyUnicode_FromString(c"not a native object".as_ptr()) };
            let borrowed = PyRef::from_owned(text).expect("a fresh unicode object");
            assert_eq!(unsafe { wrapper::native(&borrowed) }, None);
        });
    }

    /// Two ids over one address are two wrappers: the registry keys
    /// identity, not the pointer.
    #[test]
    #[ignore = "runs in its own process: see SCENARIOS"]
    fn scenario_different_ids_are_distinct_wrappers() {
        interpreter_scenario(&InitConfig::default(), |interpreter| {
            let native = 0x3000 as *mut std::os::raw::c_void;
            let left = unsafe { wrapper::wrap(native, 301) }.expect("wrap the first identity");
            let right = unsafe { wrapper::wrap(native, 302) }.expect("wrap the second");
            assert_ne!(left, right);
            let main = unsafe { capi::PyImport_AddModule(c"__main__".as_ptr()) };
            unsafe {
                capi::PyObject_SetAttrString(main, c"left".as_ptr(), left.as_ptr());
                capi::PyObject_SetAttrString(main, c"right".as_ptr(), right.as_ptr());
            }
            assert_eq!(probe(interpreter, "__probe = (left is right)"), "False");
        });
    }

    /// The cache holds exactly one reference per registered identity:
    /// wrapping is a clone (a fresh reference), dropping it goes back to
    /// the cache's own.
    #[test]
    #[ignore = "runs in its own process: see SCENARIOS"]
    fn scenario_the_cache_holds_one_reference_per_identity() {
        interpreter_scenario(&InitConfig::default(), |_interpreter| {
            let native = 0x4000 as *mut std::os::raw::c_void;
            let first = unsafe { wrapper::wrap(native, 400) }.expect("wrap");
            // The cache owns one and the caller owns one.
            assert_eq!(unsafe { capi::refcount(first.as_ptr()) }, 2);
            {
                let second = unsafe { wrapper::wrap(native, 400) }.expect("wrap again");
                assert_eq!(unsafe { capi::refcount(second.as_ptr()) }, 3);
            }
            assert_eq!(unsafe { capi::refcount(first.as_ptr()) }, 2);
            let main = unsafe { capi::PyImport_AddModule(c"__main__".as_ptr()) };
            unsafe {
                capi::PyObject_SetAttrString(main, c"kept".as_ptr(), first.as_ptr());
            }
            // `__main__` holds a third reference while the interpreter is
            // up; the cache's release happens at fini, before the
            // finaliser.
        });
    }

    /// The native type the binding scenarios bind: one attribute with a
    /// setter, one read-only attribute and one `METH_VARARGS` method -- the
    /// shape of one reference class' getset table and method table. The
    /// handlers read a process global instead of the instance, the port's
    /// stand-in for the reference's embedded-header `this` pointer.
    mod demo {
        use std::os::raw::c_int;
        use std::sync::atomic::{AtomicI64, Ordering};

        use crate::capi;

        static VALUE: AtomicI64 = AtomicI64::new(4);

        /// The demo type's description, one name for the whole process.
        pub(super) fn spec() -> crate::binding::TypeSpec {
            crate::binding::TypeSpec::new("boundless.Demo")
                .attribute("value", value_get, Some(value_set))
                .attribute("label", label_get, None)
                .method("twist", twist)
        }

        /// `value`, read: the reference's `pyGetAttribute` hands the call to
        /// this descriptor.
        unsafe extern "C" fn value_get(
            _self_: *mut capi::PyObject,
            _closure: *mut std::os::raw::c_void,
        ) -> *mut capi::PyObject {
            // SAFETY: the getter only builds a new long from a stored value.
            unsafe { capi::PyLong_FromLong(VALUE.load(Ordering::SeqCst) as std::os::raw::c_long) }
        }

        /// `value`, write: an unparseable value keeps the stored one and
        /// returns negative with the error CPython already set. A delete
        /// reaches the same setter with a null value -- `pyDelAttribute`
        /// is `PyObject_GenericSetAttr(pObject, pAttr, NULL)` -- and is
        /// refused the way the C API convention refuses it.
        unsafe extern "C" fn value_set(
            _self_: *mut capi::PyObject,
            value: *mut capi::PyObject,
            _closure: *mut std::os::raw::c_void,
        ) -> c_int {
            if value.is_null() {
                // SAFETY: the refusal only sets the error.
                unsafe {
                    capi::PyErr_SetString(
                        capi::PyExc_TypeError,
                        c"cannot delete the value attribute".as_ptr(),
                    );
                }
                return -1;
            }
            let parsed = unsafe { capi::PyLong_AsLong(value) };
            // A value of -1 with no pending error is -1 itself, not a
            // failure: only an actual conversion error returns negative.
            let failed = unsafe { !capi::PyErr_Occurred().is_null() };
            if parsed == -1 && failed {
                return -1;
            }
            VALUE.store(parsed as i64, Ordering::SeqCst);
            0
        }

        /// `label`, read-only: no setter, so `pySetAttribute` refuses it.
        unsafe extern "C" fn label_get(
            _self_: *mut capi::PyObject,
            _closure: *mut std::os::raw::c_void,
        ) -> *mut capi::PyObject {
            // SAFETY: the getter only builds a new string.
            unsafe { capi::PyUnicode_FromString(c"fixture".as_ptr()) }
        }

        /// `twist`, the agreed-convention method: the tuple is parsed with a
        /// `PyArg_ParseTuple` format string and a wrong argument raises the
        /// `TypeError` the API sets, never a panic.
        unsafe extern "C" fn twist(
            _self_: *mut capi::PyObject,
            args: *mut capi::PyObject,
        ) -> *mut capi::PyObject {
            let mut amount: c_int = 0;
            // SAFETY: the parse leaves either a value or a pending error.
            let parsed = unsafe { capi::PyArg_ParseTuple(args, c"i:twist".as_ptr(), &mut amount) };
            if parsed == 0 {
                return std::ptr::null_mut();
            }
            unsafe { capi::PyLong_FromLong(amount as i64 * 2) }
        }
    }

    /// The reference's attribute protocol end to end: a bound attribute
    /// round-trips through its getter and setter, so a script can read and
    /// write native state the way `Personality.onInit` reads class state.
    #[test]
    #[ignore = "runs in its own process: see SCENARIOS"]
    fn scenario_a_bound_attribute_round_trips() {
        interpreter_scenario(&InitConfig::default(), |interpreter| {
            let demo_type = binding::create_type(&demo::spec()).expect("build the demo type");
            // The engine builds the instance: scripts may not.
            let instance = unsafe { binding::instantiate(&demo_type) }.expect("the instance");
            let main = unsafe { capi::PyImport_AddModule(c"__main__".as_ptr()) };
            unsafe {
                capi::PyObject_SetAttrString(main, c"demo".as_ptr(), instance.as_ptr());
            }
            assert_eq!(probe(interpreter, "__probe = demo.value"), "4");
            assert_eq!(
                probe(interpreter, "demo.value = 7; __probe = (demo.value == 7)"),
                "True"
            );
        });
    }

    /// The reference's three rejections: assigning a read-only attribute is
    /// the `PY_RO_ATTRIBUTE_SET` `TypeError` naming the attribute and the
    /// `tp_name` (`pyobject_plus.hpp:443-454`), reading an unknown one stays
    /// the interpreter's `AttributeError`, and deleting a bound one is the
    /// `pyDelAttribute` path, which a getset table answers with `TypeError`.
    #[test]
    #[ignore = "runs in its own process: see SCENARIOS"]
    fn scenario_a_bound_type_refuses_the_wrong_access() {
        interpreter_scenario(&InitConfig::default(), |interpreter| {
            let demo_type = binding::create_type(&demo::spec()).expect("build the demo type");
            let instance = unsafe { binding::instantiate(&demo_type) }.expect("the instance");
            let main = unsafe { capi::PyImport_AddModule(c"__main__".as_ptr()) };
            unsafe {
                capi::PyObject_SetAttrString(main, c"demo".as_ptr(), instance.as_ptr());
            }
            assert_eq!(
                probe(
                    interpreter,
                    "try:\n    demo.label = 1\nexcept TypeError as error:\n    __probe = str(error)",
                ),
                "Sorry, the attribute label in boundless.Demo is read-only"
            );
            assert_eq!(
                probe(
                    interpreter,
                    "try:\n    demo.missing\nexcept AttributeError:\n    __probe = True\n\
                     else:\n    __probe = False",
                ),
                "True"
            );
            assert_eq!(
                probe(
                    interpreter,
                    "try:\n    del demo.value\nexcept TypeError as error:\n    __probe = str(error)",
                ),
                "cannot delete the value attribute"
            );
        });
    }

    /// The agreed convention for a native method: a good argument parses and
    /// returns, a wrong count or a wrong type raises `TypeError` from the
    /// `PyArg_ParseTuple` call, never a Rust panic.
    #[test]
    #[ignore = "runs in its own process: see SCENARIOS"]
    fn scenario_a_bound_method_parses_its_arguments() {
        interpreter_scenario(&InitConfig::default(), |interpreter| {
            let demo_type = binding::create_type(&demo::spec()).expect("build the demo type");
            let instance = unsafe { binding::instantiate(&demo_type) }.expect("the instance");
            let main = unsafe { capi::PyImport_AddModule(c"__main__".as_ptr()) };
            unsafe {
                capi::PyObject_SetAttrString(main, c"demo".as_ptr(), instance.as_ptr());
            }
            assert_eq!(probe(interpreter, "__probe = demo.twist(5)"), "10");
            assert_eq!(
                probe(
                    interpreter,
                    "try:\n    demo.twist()\nexcept TypeError:\n    __probe = True\n\
                     else:\n    __probe = False",
                ),
                "True"
            );
            assert_eq!(
                probe(
                    interpreter,
                    "try:\n    demo.twist('not a number')\nexcept TypeError:\n    __probe = True\n\
                     else:\n    __probe = False",
                ),
                "True"
            );
        });
    }

    /// `_pyNew`: a script cannot construct a native type, and the error
    /// carries the reference's message with the dotted type name.
    #[test]
    #[ignore = "runs in its own process: see SCENARIOS"]
    fn scenario_a_script_cannot_construct_a_native_type() {
        interpreter_scenario(&InitConfig::default(), |interpreter| {
            let demo_type = binding::create_type(&demo::spec()).expect("build the demo type");
            let main = unsafe { capi::PyImport_AddModule(c"__main__".as_ptr()) };
            unsafe {
                capi::PyObject_SetAttrString(main, c"native".as_ptr(), demo_type.as_ptr());
            }
            assert_eq!(
                probe(
                    interpreter,
                    "try:\n    native()\nexcept TypeError as error:\n    __probe = \
                     'Cannot directly construct objects of this type' in str(error)\n\
                     else:\n    __probe = False",
                ),
                "True"
            );
            // The refusal names the type, `%s()` with the dotted name.
            assert_eq!(
                probe(
                    interpreter,
                    "try:\n    native()\nexcept TypeError as error:\n    __probe = \
                     str(error).startswith('boundless.Demo()')\nelse:\n    __probe = False",
                ),
                "True"
            );
        });
    }

    /// `pyRepr`: the instance renders as the dotted type name and its
    /// address, the reference's `"%s at %p"` with this platform's `%p`.
    #[test]
    #[ignore = "runs in its own process: see SCENARIOS"]
    fn scenario_an_instance_renders_with_the_reference_format() {
        interpreter_scenario(&InitConfig::default(), |interpreter| {
            let demo_type = binding::create_type(&demo::spec()).expect("build the demo type");
            let instance = unsafe { binding::instantiate(&demo_type) }.expect("the instance");
            let main = unsafe { capi::PyImport_AddModule(c"__main__".as_ptr()) };
            unsafe {
                capi::PyObject_SetAttrString(main, c"demo".as_ptr(), instance.as_ptr());
            }
            assert_eq!(
                probe(
                    interpreter,
                    "__probe = repr(demo).startswith('boundless.Demo at 0x')",
                ),
                "True"
            );
        });
    }

    /// The `s_type_` rule: one name is one type object for the process, and
    /// every instance the engine builds is a distinct allocation of it.
    #[test]
    #[ignore = "runs in its own process: see SCENARIOS"]
    fn scenario_a_type_name_creates_one_type_object() {
        interpreter_scenario(&InitConfig::default(), |_interpreter| {
            let first = binding::create_type(&demo::spec()).expect("build the demo type");
            let second = binding::create_type(&demo::spec()).expect("the same name again");
            assert_eq!(first.as_ptr(), second.as_ptr());
            let left = unsafe { binding::instantiate(&first) }.expect("one instance");
            let right = unsafe { binding::instantiate(&first) }.expect("another instance");
            assert_ne!(left.as_ptr(), right.as_ptr());
            // A type this module never created answers through the C API
            // instead of the registry.
            let foreign = unsafe { capi::Py_TYPE(capi::py_true()) };
            assert_eq!(unsafe { binding::qualified_name(foreign) }, "bool");
        });
    }

    /// The acceptance probe: a script's writes through the proxy land in
    /// the native state, and its reads report the native state -- the
    /// `PySTLSequence` contract, a mutation seen from Python applied to the
    /// native object.
    #[test]
    #[ignore = "runs in its own process: see SCENARIOS"]
    fn scenario_a_sequence_proxy_round_trips_native_state() {
        interpreter_scenario(&InitConfig::default(), |interpreter| {
            let (mut _state, view) = sequence_view(&[10, 20, 30], true);
            let main = unsafe { capi::PyImport_AddModule(c"__main__".as_ptr()) };
            unsafe {
                capi::PyObject_SetAttrString(main, c"seq".as_ptr(), view.as_ptr());
            }
            assert_eq!(probe(interpreter, "__probe = len(seq)"), "3");
            assert_eq!(probe(interpreter, "__probe = seq[1]"), "20");
            probe(interpreter, "seq[1] = 25");
            assert_eq!(probe(interpreter, "__probe = seq[1]"), "25");
            // The native state carries the script's write.
            assert_eq!(_state.values(), &[10, 25, 30]);
            // And the script sees a native-side write.
            _state.values_mut()[0] = 5;
            assert_eq!(probe(interpreter, "__probe = seq[0]"), "5");
        });
    }

    /// Two views of one state share it: the reference's every view of one
    /// `PyObjectPlus` holder reports on the same native container.
    #[test]
    #[ignore = "runs in its own process: see SCENARIOS"]
    fn scenario_two_sequence_views_share_native_state() {
        interpreter_scenario(&InitConfig::default(), |interpreter| {
            let (mut state, left) = sequence_view(&[1, 2], true);
            let right = unsafe { sequence::wrap(state.as_mut() as *mut sequence::SequenceState) }
                .expect("the second view");
            let main = unsafe { capi::PyImport_AddModule(c"__main__".as_ptr()) };
            unsafe {
                capi::PyObject_SetAttrString(main, c"left".as_ptr(), left.as_ptr());
                capi::PyObject_SetAttrString(main, c"right".as_ptr(), right.as_ptr());
            }
            probe(interpreter, "left[0] = 9");
            assert_eq!(probe(interpreter, "__probe = right[0]"), "9");
            assert_eq!(probe(interpreter, "__probe = (left is right)"), "False");
        });
    }

    /// The bridge's negative-index normalisation, `seqIndexKey` on the
    /// mapping path: `seq[-1]` is the last item, and a deletion through a
    /// negative index removes the same item a positive one would.
    #[test]
    #[ignore = "runs in its own process: see SCENARIOS"]
    fn scenario_a_sequence_proxy_normalises_negative_indices() {
        interpreter_scenario(&InitConfig::default(), |interpreter| {
            let (state, view) = sequence_view(&[1, 2, 3], true);
            let main = unsafe { capi::PyImport_AddModule(c"__main__".as_ptr()) };
            unsafe {
                capi::PyObject_SetAttrString(main, c"seq".as_ptr(), view.as_ptr());
            }
            assert_eq!(probe(interpreter, "__probe = seq[-1]"), "3");
            probe(interpreter, "del seq[-1]");
            assert_eq!(state.values(), &[1, 2]);
            assert_eq!(probe(interpreter, "__probe = seq[-1]"), "2");
        });
    }

    /// Out-of-range reads and writes raise the reference's two index
    /// errors, verbatim with the type name in the `%s`.
    #[test]
    #[ignore = "runs in its own process: see SCENARIOS"]
    fn scenario_a_sequence_proxy_bounds_are_checked() {
        interpreter_scenario(&InitConfig::default(), |interpreter| {
            let (_state, view) = sequence_view(&[1], true);
            let main = unsafe { capi::PyImport_AddModule(c"__main__".as_ptr()) };
            unsafe {
                capi::PyObject_SetAttrString(main, c"seq".as_ptr(), view.as_ptr());
            }
            assert_eq!(
                probe(
                    interpreter,
                    "try:\n    seq[3]\nexcept IndexError as error:\n    \
                     __probe = type(error).__name__ + ': ' + str(error)",
                ),
                "IndexError: boundless.Sequence index out of range"
            );
            assert_eq!(
                probe(
                    interpreter,
                    "try:\n    seq[3] = 4\nexcept IndexError as error:\n    \
                     __probe = type(error).__name__ + ': ' + str(error)",
                ),
                "IndexError: boundless.Sequence assignment index out of range"
            );
        });
    }

    /// A read-only state refuses the write with the reference's message.
    #[test]
    #[ignore = "runs in its own process: see SCENARIOS"]
    fn scenario_a_read_only_sequence_refuses_assignment() {
        interpreter_scenario(&InitConfig::default(), |interpreter| {
            let (_state, view) = sequence_view(&[1], false);
            let main = unsafe { capi::PyImport_AddModule(c"__main__".as_ptr()) };
            unsafe {
                capi::PyObject_SetAttrString(main, c"seq".as_ptr(), view.as_ptr());
            }
            assert_eq!(
                probe(
                    interpreter,
                    "try:\n    seq[0] = 2\nexcept TypeError as error:\n    \
                     __probe = type(error).__name__ + ': ' + str(error)",
                ),
                "TypeError: Cannot assign an item in a read-only boundless.Sequence"
            );
        });
    }

    /// A non-integer subscript raises the reference's `TypeError`, and a
    /// foreign object through the bridge raises the reference's
    /// not-subscriptable refusal: the message a plain `str` produces is the
    /// `PyType_GetName` arm of the qualified-name lookup.
    #[test]
    #[ignore = "runs in its own process: see SCENARIOS"]
    fn scenario_a_non_integer_subscript_refuses() {
        interpreter_scenario(&InitConfig::default(), |interpreter| {
            let (_state, view) = sequence_view(&[1], true);
            let main = unsafe { capi::PyImport_AddModule(c"__main__".as_ptr()) };
            unsafe {
                capi::PyObject_SetAttrString(main, c"seq".as_ptr(), view.as_ptr());
            }
            assert_eq!(
                probe(
                    interpreter,
                    "try:\n    seq['one']\nexcept TypeError as error:\n    \
                     __probe = type(error).__name__ + ': ' + str(error)",
                ),
                "TypeError: boundless.Sequence indices must be integers, not str"
            );
            // The bridge's own miss arm renders the same sentence; CPython's
            // own wording for a `str` here is the independent check of it.
            assert_eq!(
                probe(
                    interpreter,
                    "try:\n    'text'[0] = 1\nexcept TypeError as error:\n    \
                     __probe = type(error).__name__ + ': ' + str(error)",
                ),
                "TypeError: 'str' object does not support item assignment"
            );
        });
    }

    /// The reference's keep-alive contract, one entry: `PySTLSequence`'s
    /// constructor `Py_XINCREF`s its owner and the destructor `Py_XDECREF`s it,
    /// so one entry is exactly one strong reference -- not two, not none.
    /// `sys.getrefcount` names the object itself as well, so the counts read
    /// two here; what the test measures is the delta of one.
    #[test]
    #[ignore = "runs in its own process: see SCENARIOS"]
    fn scenario_a_keep_alive_entry_holds_one_reference() {
        interpreter_scenario(&InitConfig::default(), |interpreter| {
            let list = fresh();
            let baseline = refcount(interpreter);
            unsafe { keepalive::pin(41, &list) }.expect("pin the entry");
            assert!(keepalive::is_pinned(41));
            assert_eq!(keepalive::pinned_count(), 1);
            assert_eq!(refcount(interpreter), baseline + 1);
            assert!(keepalive::unpin(41));
            assert_eq!(refcount(interpreter), baseline);
        });
    }

    /// Repinning an identity adopts the new reference and drops the old one
    /// exactly once, `Script::setData`'s rule: no leak on replace, no entry
    /// for an identity that no longer exists.
    #[test]
    #[ignore = "runs in its own process: see SCENARIOS"]
    fn scenario_repinning_adopts_and_replaces_once() {
        interpreter_scenario(&InitConfig::default(), |_interpreter| {
            let first = fresh();
            unsafe { keepalive::pin(9, &first) }.expect("pin the first");
            // The parked name and the handle hold two; the entry is the
            // third. Read directly, because the second pin replaces the
            // object the `__handle` helper reads.
            assert_eq!(unsafe { capi::refcount(first.as_ptr()) }, 3);
            // SAFETY: the interpreter is up and this thread owns the GIL.
            let second = unsafe { PyRef::from_owned(capi::PyList_New(0)) }.expect("a live object");
            unsafe { keepalive::pin(9, &second) }.expect("replace the entry");
            // One entry, not two: the replace is an adopt.
            assert_eq!(keepalive::pinned_count(), 1);
            assert!(keepalive::is_pinned(9));
            // The first entry's reference went exactly once, `setData`'s rule:
            // the count is back to the two the script side still holds. A
            // double release would have freed it under the parked name; a
            // missing one would have left it at three.
            assert_eq!(unsafe { capi::refcount(first.as_ptr()) }, 2);
        });
    }

    /// Unpinning twice releases once: the second call finds no entry and
    /// reports it, which is what makes a host's double `fini` harmless.
    #[test]
    #[ignore = "runs in its own process: see SCENARIOS"]
    fn scenario_unpinning_twice_releases_once() {
        interpreter_scenario(&InitConfig::default(), |interpreter| {
            let object = fresh();
            unsafe { keepalive::pin(7, &object) }.expect("pin");
            let before = refcount(interpreter);
            assert!(keepalive::unpin(7));
            assert_eq!(refcount(interpreter), before - 1);
            assert!(!keepalive::unpin(7));
            assert!(!keepalive::is_pinned(7));
            assert_eq!(refcount(interpreter), before - 1);
        });
    }

    /// `Script::gTimers`'s contract: the native registry's entry keeps a
    /// callback alive after the script has thrown it away, and unpinning is
    /// what finally releases it -- the timer table drops its owned function
    /// and arguments at `clearTimers`, never earlier.
    #[test]
    #[ignore = "runs in its own process: see SCENARIOS"]
    fn scenario_a_pinned_callback_survives_the_script_dropping_it() {
        interpreter_scenario(&InitConfig::default(), |interpreter| {
            // A bound method as the held callback, the shape the timer table
            // stores: the instance and the function together.
            assert!(interpreter.run_simple_string(
                "class Holder:\n    def answer(self):\n        return 6\n\
                 \nh = Holder()\n__callback = h.answer"
            ));
            // SAFETY: the interpreter is up and this thread owns the GIL.
            let callback = unsafe {
                let main = capi::PyImport_AddModule(c"__main__".as_ptr());
                PyRef::from_owned(capi::PyObject_GetAttrString(main, c"__callback".as_ptr()))
                    .expect("the bound method")
            };
            unsafe { keepalive::pin(1, &callback) }.expect("pin the callback");
            // The script lets go of every name it had.
            assert!(interpreter.run_simple_string("del h\ndel __callback"));
            // A collection freed nothing, because the entry owns one
            // reference of its own.
            assert!(interpreter.run_simple_string("import gc; gc.collect()"));
            // The handle and the entry are the only two names left: the entry
            // is what the script's drop could not kill.
            assert_eq!(unsafe { capi::refcount(callback.as_ptr()) }, 2);
            // The entry is still callable: the registry's reference is
            // the only thing keeping it alive.
            let held = keepalive::take_out(1).expect("the held reference");
            // The take-out handed the entry's reference to the caller, the
            // release-happened-exactly-once leg of the contract: the caller's
            // drop is what frees it.
            drop(callback);
            // The reference's `MF_ASSERT_DEV(ob_refcnt == 0)` at native
            // teardown, read the way the C++ layer reads it: one owned
            // reference is left, so the count is not zero yet -- and it is
            // the caller's final drop that takes it there, never the registry
            // releasing twice.
            assert!(!unsafe { keepalive::zero_reference_count(&held) });
            assert_eq!(unsafe { capi::refcount(held.as_ptr()) }, 1);
            let answer = unsafe { capi::PyObject_CallNoArgs(held.as_ptr()) };
            assert!(!answer.is_null());
            // SAFETY: the answer is a fresh strong reference from the call.
            unsafe { capi::dec_ref(answer) };
            drop(held);
        });
    }

    /// The reference's collection policy: `gc.disable()` in init means the
    /// automatic collector never runs, so a cycle the script builds is freed
    /// only by an explicit pass -- `gc.collect()` here, and the same
    /// `while (PyGC_Collect() > 0);` loop that `Script::fini` runs after
    /// `__main__` is blanked. Either way the rule under test is the same:
    /// the two members of the cycle go exactly once, never twice, never
    /// early while the script still names them.
    #[test]
    #[ignore = "runs in its own process: see SCENARIOS"]
    fn scenario_a_script_side_cycle_is_freed_exactly_once() {
        interpreter_scenario(&InitConfig::default(), |interpreter| {
            assert_eq!(
                probe(interpreter, "import gc; __probe = gc.isenabled()"),
                "False"
            );
            // A two-object cycle, each naming the other.
            assert!(interpreter.run_simple_string("a = []\nb = []\na.append(b)\nb.append(a)"));
            // Collection is off, so nothing collects on its own: an explicit
            // pass is what frees the cycle. One pass warms up the leftovers
            // init left unreachable, so the counts afterwards are about the
            // cycle alone.
            assert_ne!(probe(interpreter, "__probe = gc.collect()"), "0");
            // While the script still names the members, a pass finds nothing
            // unreachable.
            assert_eq!(probe(interpreter, "__probe = gc.collect()"), "0");
            // The script drops every name; only the cycle keeps itself alive.
            assert!(interpreter.run_simple_string("del a\ndel b"));
            assert_eq!(
                probe(interpreter, "__probe = gc.collect()"),
                "2",
                "the two-list cycle was freed"
            );
            // A second pass frees nothing: the cycle went exactly once, not
            // twice and not zero times.
            assert_eq!(probe(interpreter, "__probe = gc.collect()"), "0");
        });
    }

    /// `s_defaultContext` is a `THREADLOCAL`, captured at init on the thread that
    /// brings the interpreter up (`script.cpp:603-604`), and `Script::fini`
    /// leaves it in place -- it only reverts the poller's idle hooks to no-ops.
    /// The context is what makes [`gil::acquire_lock`] and
    /// [`gil::release_lock`] restore and save this thread's state instead of
    /// doing nothing.
    #[test]
    #[ignore = "runs in its own process: see SCENARIOS"]
    fn scenario_the_gil_context_is_installed_at_init() {
        interpreter_scenario(&InitConfig::default(), |interpreter| {
            assert!(gil::has_context());
            assert!(interpreter.run_simple_string("seen = 'script'"));
            // The thread that brought the interpreter up already holds the GIL,
            // so CPython reports it as the current thread state.
            let installed = unsafe { capi::PyThreadState_Get() };
            assert!(!installed.is_null());
            unsafe {
                // `releaseLock` gives the GIL back and reports `Ok` because
                // `PyEval_SaveThread` handed back exactly the state this
                // thread was given. It does not remove the context: only
                // `finiThread` does, so the thread still names a state while
                // holding no current one.
                assert!(gil::release_lock().is_ok());
                assert!(gil::has_context());
                assert_eq!(capi::PyGILState_Check(), 0, "the lock went with it");
                // `acquireLock` puts this thread back on the state it was
                // given, so a call the script thread makes still sees its own
                // globals, the way `Script::call` assumes.
                gil::acquire_lock();
                assert_ne!(capi::PyGILState_Check(), 0, "the lock came back");
                assert_eq!(capi::PyThreadState_Get(), installed);
            }
            assert!(gil::has_context());
            assert_eq!(probe(interpreter, "__probe = seen"), "script");
        });
    }

    /// The cross-thread contract: a callback posted from a thread that then
    /// exits is the queue's to run, not the thread's to keep, so it still
    /// reaches the script thread -- and the script thread is the one that
    /// runs it, on the interpreter the caller owns.
    #[test]
    #[ignore = "runs in its own process: see SCENARIOS"]
    fn scenario_a_callback_posted_from_a_dying_thread_runs_on_the_script_thread() {
        interpreter_scenario(&InitConfig::default(), |interpreter| {
            assert!(interpreter.run_simple_string("counter = [0]"));
            let ran = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            let spawner = ran.clone();
            let poster = std::thread::spawn(move || {
                gil::post(move || {
                    // The callback runs on the thread that owns the GIL, so
                    // it may touch the interpreter it cannot send across.
                    // SAFETY: `process` requires the caller to own the GIL, and
                    // this closure only ever runs there.
                    let Ok(source) = std::ffi::CString::new("counter[0] = counter[0] + 1") else {
                        return;
                    };
                    unsafe { capi::PyRun_SimpleString(source.as_ptr()) };
                    spawner.store(true, std::sync::atomic::Ordering::SeqCst);
                })
            });
            // The thread is over; its callback is still queued.
            assert!(poster.join().is_ok());
            assert_eq!(gil::pending(), 1);
            assert!(!ran.load(std::sync::atomic::Ordering::SeqCst));
            assert_eq!(unsafe { gil::process() }, 1);
            assert!(ran.load(std::sync::atomic::Ordering::SeqCst));
            assert_eq!(gil::pending(), 0);
            assert_eq!(probe(interpreter, "__probe = counter[0]"), "1");
        });
    }

    /// A callback whose script raises does not strand the callbacks posted
    /// behind it: the pass has already taken its snapshot out of the queue, so
    /// the failure is contained to that one callback.
    #[test]
    #[ignore = "runs in its own process: see SCENARIOS"]
    fn scenario_an_exception_in_a_callback_does_not_leave_it_queued() {
        interpreter_scenario(&InitConfig::default(), |interpreter| {
            assert!(interpreter.run_simple_string("counter = [0]"));
            let failing = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            let observer = failing.clone();
            let source = "raise ValueError('the callback fails')";
            gil::post(move || {
                // SAFETY: this runs on the GIL-owning thread.
                let Ok(encoded) = std::ffi::CString::new(source) else {
                    return;
                };
                // The raise is caught on the script side, the way the
                // reference's callbacks catch theirs; the point is that the
                // exception does not unwind the pass.
                unsafe {
                    capi::PyRun_SimpleString(encoded.as_ptr());
                }
                observer.store(true, std::sync::atomic::Ordering::SeqCst);
            });
            gil::post(|| {
                // SAFETY: this runs on the GIL-owning thread.
                let Ok(source) = std::ffi::CString::new("counter[0] = counter[0] + 1") else {
                    return;
                };
                unsafe { capi::PyRun_SimpleString(source.as_ptr()) };
            });
            assert_eq!(unsafe { gil::process() }, 2);
            assert!(failing.load(std::sync::atomic::Ordering::SeqCst));
            assert_eq!(gil::pending(), 0);
            assert_eq!(probe(interpreter, "__probe = counter[0]"), "1");
        });
    }

    /// `Script::clearTimers`'s job: a callback posted but never run must not
    /// be left behind the script system, so shutdown drops it rather than
    /// handing it to a torn-down interpreter.
    #[test]
    #[ignore = "runs in its own process: see SCENARIOS"]
    fn scenario_finishing_the_script_system_leaves_no_callback_behind() {
        let config = scenario_config(&InitConfig::default());
        let interpreter = Interpreter::init(&config).expect("Script::init");
        let dropped = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let marker = dropped.clone();
        gil::post(move || {
            marker.store(true, std::sync::atomic::Ordering::SeqCst);
        });
        assert_eq!(gil::pending(), 1);
        interpreter.fini();
        assert_eq!(gil::pending(), 0, "nothing survives the drain");
        assert!(!dropped.load(std::sync::atomic::Ordering::SeqCst));
        assert!(Interpreter::is_finalized());
    }

    /// `Script::init` drains the init-time jobs itself, after the
    /// interpreter is up but before `gc.disable()` -- the reference's
    /// `runInitTimeJobs()` position at `script.cpp:584`.
    #[test]
    #[ignore = "runs in its own process: see SCENARIOS"]
    fn scenario_init_runs_the_init_time_jobs() {
        let ran = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let gc_was_enabled = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        {
            let ran = ran.clone();
            let gc_was_enabled = gc_was_enabled.clone();
            assert_eq!(
                init_time_job::add_init_time_job(0, move || {
                    ran.store(true, std::sync::atomic::Ordering::SeqCst);
                    // A failing job leaves no pending exception behind: the
                    // helper cleared it, and the good probe below still runs.
                    assert!(!run_simple_probe("import no_such_probe_module"));
                    // A source with an embedded NUL cannot even be encoded,
                    // and a source that assigns no `__probe` reads as false.
                    assert!(!run_simple_probe("has\u{0}an_embedded_nul"));
                    assert!(!run_simple_probe("pass"));
                    // SAFETY: the interpreter is up inside Script::init and
                    // this runs on the thread that owns the GIL.
                    let enabled = run_simple_probe("import gc; __probe = gc.isenabled()");
                    gc_was_enabled.store(enabled, std::sync::atomic::Ordering::SeqCst);
                }),
                Ok(())
            );
        }
        let interpreter =
            Interpreter::init(&scenario_config(&InitConfig::default())).expect("Script::init");
        assert!(ran.load(std::sync::atomic::Ordering::SeqCst));
        assert!(gc_was_enabled.load(std::sync::atomic::Ordering::SeqCst));
        // After init, collection is off: the job really did run before it.
        assert_eq!(
            probe(&interpreter, "import gc; __probe = gc.isenabled()"),
            "False"
        );
        interpreter.fini();
    }

    /// A `print` on stdout lands in the log sink as an info-level line,
    /// the reference's `PythonScriptOutputWriter_testStdioDebugMessage`.
    #[test]
    #[ignore = "runs in its own process: see SCENARIOS"]
    fn scenario_stdout_lands_in_the_log_sink() {
        let lines = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        {
            let lines = lines.clone();
            output_writer::set_log_sink(move |line, is_stderr| {
                lines.lock().unwrap().push((line.to_owned(), is_stderr));
            });
        }
        interpreter_scenario(&InitConfig::default(), |interpreter| {
            assert!(interpreter.run_simple_string("print('Test stdio string: 25')"));
            assert_eq!(
                *lines.lock().unwrap(),
                vec![("Test stdio string: 25".to_owned(), false)]
            );
            // The stream still looks like a stream to the script: softspace
            // is present and writable, as the reference's PY_RW attribute.
            assert_eq!(
                probe(
                    interpreter,
                    "import sys; sys.stdout.softspace = True; \
                     __probe = sys.stdout.softspace"
                ),
                "True"
            );
        });
    }

    /// stderr carries the error level, the reference's
    /// `PythonScriptOutputWriter_testStderrDebugMessage`.
    #[test]
    #[ignore = "runs in its own process: see SCENARIOS"]
    fn scenario_stderr_lands_in_the_log_sink() {
        let lines = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        {
            let lines = lines.clone();
            output_writer::set_log_sink(move |line, is_stderr| {
                lines.lock().unwrap().push((line.to_owned(), is_stderr));
            });
        }
        interpreter_scenario(&InitConfig::default(), |interpreter| {
            assert!(
                interpreter.run_simple_string(
                    "import sys; print('Test stderr string: 45', file=sys.stderr)"
                )
            );
            assert_eq!(
                *lines.lock().unwrap(),
                vec![("Test stderr string: 45".to_owned(), true)]
            );
        });
    }

    /// Writes without their newline hold in the stream buffer until the
    /// newline arrives, while hooks receive the raw chunks either way --
    /// and hooks need the redirect up before they can latch.
    #[test]
    #[ignore = "runs in its own process: see SCENARIOS"]
    fn scenario_partial_writes_buffer_and_hooks_see_chunks() {
        let lines = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let chunks = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        {
            let lines = lines.clone();
            output_writer::set_log_sink(move |line, is_stderr| {
                lines.lock().unwrap().push((line.to_owned(), is_stderr));
            });
        }
        interpreter_scenario(&InitConfig::default(), |interpreter| {
            let chunks_handle = chunks.clone();
            output_writer::add_hook(Box::new(move |chunk: &str, is_stderr: bool| {
                chunks_handle
                    .lock()
                    .unwrap()
                    .push((chunk.to_owned(), is_stderr));
            }))
            .expect("hook the redirect");
            assert!(interpreter.run_simple_string("import sys; sys.stdout.write('hel')"));
            assert!(interpreter.run_simple_string("import sys; sys.stdout.write('lo')"));
            assert!(lines.lock().unwrap().is_empty());
            assert_eq!(
                *chunks.lock().unwrap(),
                vec![("hel".to_owned(), false), ("lo".to_owned(), false)]
            );
            assert!(interpreter.run_simple_string("import sys; sys.stdout.write('\\n')"));
            assert_eq!(*lines.lock().unwrap(), vec![("hello".to_owned(), false)]);
        });
        // The redirect is gone after fini, so late hooks are refused.
        assert_eq!(
            output_writer::add_hook(Box::new(|_: &str, _: bool| {})),
            Err(output_writer::HookError::NoStdioWriter)
        );
    }

    /// A hook can remove itself from inside its own callback without
    /// invalidating the iteration, the reference's `pCurrentHook_` dance;
    /// hooks after it keep receiving everything.
    #[test]
    #[ignore = "runs in its own process: see SCENARIOS"]
    fn scenario_a_hook_may_remove_itself_mid_callback() {
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen_after = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let own_id = std::sync::Arc::new(std::sync::Mutex::new(None));
        interpreter_scenario(&InitConfig::default(), |interpreter| {
            let seen_handle = seen.clone();
            let own_id_handle = own_id.clone();
            let id = output_writer::add_hook(Box::new(move |chunk: &str, is_stderr: bool| {
                seen_handle
                    .lock()
                    .unwrap()
                    .push((chunk.to_owned(), is_stderr));
                if chunk == "two\n" {
                    // From inside a callback only the running hook may
                    // remove anything: another identity is refused.
                    assert_eq!(
                        output_writer::remove_hook(u64::MAX),
                        Err(output_writer::HookError::NoSuchHook)
                    );
                    // The self-removal: this very callback is the one
                    // running, so the removal must wait for it to return.
                    if let Some(id) = *own_id_handle.lock().unwrap() {
                        assert_eq!(output_writer::remove_hook(id), Ok(()));
                    }
                }
            }))
            .expect("hook the redirect");
            // The hook only reads this once it is running, which is after
            // `add_hook` has handed back the identity.
            *own_id.lock().unwrap() = Some(id);
            let after_handle = seen_after.clone();
            let follower =
                output_writer::add_hook(Box::new(move |chunk: &str, is_stderr: bool| {
                    after_handle
                        .lock()
                        .unwrap()
                        .push((chunk.to_owned(), is_stderr));
                }))
                .expect("hook the follower");
            for word in ["one", "two", "three"] {
                assert!(
                    interpreter
                        .run_simple_string(&format!("import sys; sys.stdout.write('{word}\\n')"))
                );
            }
            assert_eq!(
                *seen.lock().unwrap(),
                vec![("one\n".to_owned(), false), ("two\n".to_owned(), false)]
            );
            assert_eq!(
                *seen_after.lock().unwrap(),
                vec![
                    ("one\n".to_owned(), false),
                    ("two\n".to_owned(), false),
                    ("three\n".to_owned(), false)
                ]
            );
            // Outside any callback the removal goes through the writer
            // lock and takes by identity; a repeat reports the missing id.
            assert_eq!(output_writer::remove_hook(follower), Ok(()));
            assert_eq!(
                output_writer::remove_hook(follower),
                Err(output_writer::HookError::NoSuchHook)
            );
        });
    }

    /// An embedded NUL is not UTF-8 text, so the repr of the argument is
    /// logged instead, the reference's `memchr` branch. A repr never ends
    /// with a real newline, so it stays in the buffer, and the next
    /// newline-terminated write flushes it joined with its own text.
    #[test]
    #[ignore = "runs in its own process: see SCENARIOS"]
    fn scenario_an_embedded_nul_logs_the_repr() {
        let lines = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        {
            let lines = lines.clone();
            output_writer::set_log_sink(move |line, is_stderr| {
                lines.lock().unwrap().push((line.to_owned(), is_stderr));
            });
        }
        interpreter_scenario(&InitConfig::default(), |interpreter| {
            assert!(interpreter.run_simple_string("import sys; sys.stdout.write('a\\x00b\\n')"));
            assert!(interpreter.run_simple_string("import sys; sys.stdout.write('tail\\n')"));
            assert_eq!(
                *lines.lock().unwrap(),
                vec![("'a\\x00b\\n'tail".to_owned(), false)]
            );
        });
    }

    /// `sys.excepthook` is the port's `printTraceBack`: a raised exception
    /// reported through it lands in the log entry -- header, frames with
    /// the source line, and the exception line -- the whole chain routed
    /// through the hooked stderr as error-level entries.
    #[test]
    #[ignore = "runs in its own process: see SCENARIOS"]
    fn scenario_the_excepthook_formats_the_traceback() {
        let lines = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        {
            let lines = lines.clone();
            output_writer::set_log_sink(move |line, is_stderr| {
                lines.lock().unwrap().push((line.to_owned(), is_stderr));
            });
        }
        let boom_path = std::env::temp_dir()
            .join(format!("boundless-python-host-{}", std::process::id()))
            .join("scripts/common/boom.py");
        interpreter_scenario(&InitConfig::default(), |interpreter| {
            assert!(interpreter.run_simple_string(
                "import sys, boom\n\
                     try:\n\
                     \x20\x20\x20\x20boom.boom()\n\
                     except ValueError:\n\
                     \x20\x20\x20\x20sys.excepthook(*sys.exc_info())"
            ));
            let logged: Vec<String> = lines
                .lock()
                .unwrap()
                .iter()
                .map(|(line, _)| line.clone())
                .collect();
            let text = logged.join("\n");
            assert!(
                text.contains("Traceback (most recent call last):"),
                "{text}"
            );
            // The fixture root's scripts/common is on sys.path, so the
            // frame names the module file it raised in.
            let boom_name = boom_path.to_str().expect("a UTF-8 path").to_owned();
            let frame = format!("  File \"{boom_name}\", line 2, in boom");
            assert!(text.contains(&frame), "{text}");
            assert!(
                text.contains("    raise ValueError('boom message')"),
                "{text}"
            );
            assert!(text.contains("ValueError: boom message"), "{text}");
            // Every traceback line came in as an error-level entry.
            assert!(
                lines
                    .lock()
                    .unwrap()
                    .iter()
                    .all(|(_, is_stderr)| *is_stderr)
            );
        });
    }

    /// An exception without a traceback still renders: the reference's
    /// fallback to `PyErr_Display`.
    #[test]
    #[ignore = "runs in its own process: see SCENARIOS"]
    fn scenario_the_excepthook_handles_a_missing_traceback() {
        let lines = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        {
            let lines = lines.clone();
            output_writer::set_log_sink(move |line, is_stderr| {
                lines.lock().unwrap().push((line.to_owned(), is_stderr));
            });
        }
        interpreter_scenario(&InitConfig::default(), |interpreter| {
            assert!(interpreter.run_simple_string(
                "import sys; sys.excepthook(ValueError, ValueError('only'), None)"
            ));
            // Fewer than three arguments fails the parse; the reference
            // logs an error, clears the exception, and returns None, so
            // nothing propagates to the caller.
            assert!(interpreter.run_simple_string("sys.excepthook(1)"));
            let text: String = lines
                .lock()
                .unwrap()
                .iter()
                .map(|(line, _)| line.clone())
                .collect::<Vec<_>>()
                .join("\n");
            assert!(text.contains("ValueError: only"), "{text}");
        });
    }

    /// A `write` call with bad arguments raises `TypeError` into the
    /// caller: the reference lets the parse failure propagate.
    #[test]
    #[ignore = "runs in its own process: see SCENARIOS"]
    fn scenario_bad_write_arguments_raise() {
        interpreter_scenario(&InitConfig::default(), |interpreter| {
            assert!(interpreter.run_simple_string(
                "\nimport sys\ntry:\n    sys.stdout.write()\nexcept TypeError:\n    pass\nelse:\n    raise AssertionError('write() did not raise')\n"
            ));
        });
    }

    /// `import bw_site` runs at the tail of init from the res tree.
    #[test]
    #[ignore = "runs in its own process: see SCENARIOS"]
    fn scenario_bw_site_is_imported_at_the_tail_of_init() {
        interpreter_scenario(&InitConfig::default(), |interpreter| {
            assert_eq!(
                probe(interpreter, "import sys; __probe = sys.bw_site_imported"),
                "True"
            );
        });
    }

    /// Without the engine's script tree on the path, `import bw_site` fails
    /// and init reports it, the reference's
    /// `"Script::init: Unable to import bw_site"` returning false.
    #[test]
    #[ignore = "runs in its own process: see SCENARIOS"]
    fn scenario_missing_bw_site_fails_init() {
        assert_eq!(
            Interpreter::init(&InitConfig::default()).err(),
            Some(InitError::BwSiteImportFailed)
        );
        assert!(!Interpreter::is_initialized());
    }

    /// Before any init the script system is neither initialised nor
    /// finalised, and asking it to finalise is a no-op rather than a
    /// `Py_Finalize` on a dead interpreter.
    #[test]
    fn the_script_system_starts_down() {
        assert!(!Interpreter::is_initialized());
        assert!(!Interpreter::is_finalized());
        Interpreter.fini();
    }

    /// The export manifest agrees with the live interpreter: the surface it
    /// marks ported imports and resolves, and its scoping records read true
    /// against the modules that exist today -- `BigWorld.component` is
    /// present, the client-only `BigWorld.callback` is not, and the ported
    /// `Math` module imports with every delivered name.
    #[test]
    #[ignore = "runs in its own process: see SCENARIOS"]
    fn scenario_the_manifest_matches_the_interpreter() {
        interpreter_scenario(&InitConfig::default(), |interpreter| {
            // SAFETY: the interpreter is up and this thread owns the GIL.
            assert_eq!(unsafe { manifest::check() }, Ok(()));
            let _ = interpreter;
            // SAFETY: the interpreter is up and this thread owns the GIL.
            let big_world = unsafe { capi::PyImport_ImportModule(c"BigWorld".as_ptr()) };
            assert!(!big_world.is_null(), "BigWorld imports");
            // SAFETY: BigWorld is a live module object.
            let component =
                unsafe { capi::PyObject_GetAttrString(big_world, c"component".as_ptr()) };
            assert!(!component.is_null(), "BigWorld.component is present");
            // SAFETY: both results are owned references from the calls above.
            unsafe {
                capi::dec_ref(component);
                // `callback` is client-only surface: its absence is the
                // scoping fact the manifest pins for BW-03-03-03.
                let callback = capi::PyObject_GetAttrString(big_world, c"callback".as_ptr());
                assert!(callback.is_null(), "BigWorld.callback is not here");
                capi::clear_error();
                capi::dec_ref(big_world);
            }
            // SAFETY: the interpreter is up and this thread owns the GIL.
            let math = unsafe { capi::PyImport_ImportModule(c"Math".as_ptr()) };
            assert!(!math.is_null(), "Math imports: the port delivered it");
            // SAFETY: math is a live module object.
            unsafe { capi::dec_ref(math) };
        });
    }

    /// The Math module serves the ported surface: the delivered factory
    /// names sit in the module while the internal coercion target, the
    /// register type and the Windows-only animation never show, a provider
    /// chain re-evaluates bottom-up on every read, a tuple coerces into the
    /// Basic provider whose `value` answers the reference's read-only
    /// sentence, the walls raise the reference's own sentences, the
    /// register file hands out one shared handle per cell, and the host
    /// tick walks the autoTick providers.
    #[test]
    #[ignore = "runs in its own process: see SCENARIOS"]
    fn scenario_the_math_module_serves_the_ported_surface() {
        interpreter_scenario(&InitConfig::default(), |interpreter| {
            // Every delivered factory name, and none of the internals: the
            // coercion target (`Vector4Basic`) and the register type are
            // link-internal in the reference, and `MatrixAnimation` is
            // Windows-only.
            assert_eq!(
                probe(
                    interpreter,
                    "import Math\n\
                     __probe = ', '.join(sorted(n for n in dir(Math) if not n.startswith('_')))",
                ),
                "Matrix, MatrixInverse, MatrixProduct, Plane, Vector2, Vector3, Vector4, \
                 Vector4Animation, Vector4Combiner, Vector4Distance, Vector4LFO, \
                 Vector4MatrixAdaptor, Vector4Morph, Vector4Product, Vector4Shader, \
                 Vector4Swizzle, Vector4Translation, getRegister",
            );
            assert_eq!(
                probe(
                    interpreter,
                    "import Math\n\
                     __probe = any(n in dir(Math) for n in \
                     ('Vector4Basic', 'Vector4Register', 'MatrixAnimation'))",
                ),
                "False",
            );

            // A product chain re-evaluates bottom-up on every read: a
            // provider assigned to an input reads through, so a later write
            // below it shows on top without a re-assign. A product with
            // only one source provides that source's value -- the rule the
            // reference documents (`script_math.cpp:3322-3325`).
            assert_eq!(
                probe(
                    interpreter,
                    "import Math\n\
                     top = Math.Vector4Product()\n\
                     bottom = Math.Vector4Product()\n\
                     bottom.b = (2, 2, 2, 2)\n\
                     top.a = bottom\n\
                     top.b = (1, 1, 1, 1)\n\
                     before = tuple(top.value)\n\
                     bottom.a = (3, 3, 3, 3)\n\
                     __probe = (before, tuple(top.value))",
                ),
                "((2.0, 2.0, 2.0, 2.0), (6.0, 6.0, 6.0, 6.0))",
            );

            // A tuple coerced into a provider slot becomes the Basic type,
            // and its `value` answers the reference's read-only sentence.
            assert_eq!(
                probe(
                    interpreter,
                    "import Math\n\
                     p = Math.Vector4Product()\n\
                     p.a = (1, 2, 3, 4)\n\
                     __probe = tuple(p.a.value)",
                ),
                "(1.0, 2.0, 3.0, 4.0)",
            );
            assert_eq!(
                probe(
                    interpreter,
                    "import Math\n\
                     p = Math.Vector4Product()\n\
                     p.a = (1, 2, 3, 4)\n\
                     try:\n\
                     \x20   p.a.value = 5\n\
                     except TypeError as error:\n\
                     \x20   __probe = str(error)",
                ),
                "Sorry, the attribute value in Math.Vector4Basic is read-only",
            );

            // The constructor wall: the factories take no script arguments.
            assert_eq!(
                probe(
                    interpreter,
                    "import Math\n\
                     try:\n\
                     \x20   Math.Vector4Product(1)\n\
                     except TypeError as error:\n\
                     \x20   __probe = str(error)",
                ),
                "Vector4Product() takes no arguments.",
            );

            // The enum walls: a string name round-trips, an unknown name is
            // the `ValueError` listing the legal ones, a non-string is the
            // same sentence as the `TypeError`.
            assert_eq!(
                probe(
                    interpreter,
                    "import Math\n\
                     lfo = Math.Vector4LFO()\n\
                     lfo.waveform = 'SINE'\n\
                     __probe = lfo.waveform",
                ),
                "SINE",
            );
            assert_eq!(
                probe(
                    interpreter,
                    "import Math\n\
                     lfo = Math.Vector4LFO()\n\
                     try:\n\
                     \x20   lfo.waveform = 'WOBBLE'\n\
                     except ValueError as error:\n\
                     \x20   __probe = str(error)",
                ),
                "waveform must be set to one of SQUARE, TRIANGLE, SINE, or SAWTOOTH",
            );
            assert_eq!(
                probe(
                    interpreter,
                    "import Math\n\
                     lfo = Math.Vector4LFO()\n\
                     try:\n\
                     \x20   lfo.waveform = 2\n\
                     except TypeError as error:\n\
                     \x20   __probe = str(error)",
                ),
                "waveform must be set to one of SQUARE, TRIANGLE, SINE, or SAWTOOTH",
            );

            // The keyframe walls: a non-sequence and a wrong pair refuse
            // with the reference's sentences, a good set lerp-walks on the
            // `time` attribute.
            assert_eq!(
                probe(
                    interpreter,
                    "import Math\n\
                     animation = Math.Vector4Animation()\n\
                     try:\n\
                     \x20   animation.keyframes = 5\n\
                     except TypeError as error:\n\
                     \x20   __probe = str(error)",
                ),
                "keyframes must be set to a sequence",
            );
            assert_eq!(
                probe(
                    interpreter,
                    "import Math\n\
                     animation = Math.Vector4Animation()\n\
                     try:\n\
                     \x20   animation.keyframes = [(0.0,)]\n\
                     except TypeError as error:\n\
                     \x20   __probe = str(error)",
                ),
                "An item of a PySTLSequence of a keyframe must be set to a tuple of \
                 a float and a Vector4Provider",
            );
            assert_eq!(
                probe(
                    interpreter,
                    "import Math\n\
                     animation = Math.Vector4Animation()\n\
                     animation.keyframes = [(0.0, (1, 2, 3, 4)), (1.0, (2, 2, 2, 2))]\n\
                     animation.time = 0.5\n\
                     __probe = tuple(animation.value)",
                ),
                "(1.5, 2.0, 2.5, 3.0)",
            );

            // The register file: one shared handle per cell, zeros until a
            // shader writes, and the cap of 62 is the reference's own
            // `MAX_ARG` sentence.
            assert_eq!(
                probe(
                    interpreter,
                    "import Math\n\
                     register = Math.getRegister(3)\n\
                     __probe = (register is Math.getRegister(3), tuple(register.value))",
                ),
                "(True, (0.0, 0.0, 0.0, 0.0))",
            );
            assert_eq!(
                probe(
                    interpreter,
                    "import Math\n\
                     try:\n\
                     \x20   Math.getRegister(63)\n\
                     except TypeError as error:\n\
                     \x20   __probe = str(error)",
                ),
                "() argument 1 is greater than maximum value",
            );

            // The provider wall: a plain number is no provider.
            assert_eq!(
                probe(
                    interpreter,
                    "import Math\n\
                     product = Math.Vector4Product()\n\
                     try:\n\
                     \x20   product.a = 5\n\
                     except TypeError as error:\n\
                     \x20   __probe = str(error)",
                ),
                "a must be set to a Vector4Provider or None",
            );

            // The autoTick providers walk under the host's per-frame tick:
            // the morph sits a hair past its old value -- `target` writes
            // `time_ = 0.f` directly (`script_math.cpp:3852-3856`) but the
            // `duration` setter re-clamps through the 0.0001 floor
            // (`:3882-3885`) -- and reaches the target halfway through its
            // duration.
            assert_eq!(
                probe(
                    interpreter,
                    "import Math\n\
                     morph = Math.Vector4Morph((0, 0, 0, 0))\n\
                     morph.target = (2, 2, 2, 2)\n\
                     morph.duration = 1.0\n\
                     __probe = tuple(round(c, 5) for c in morph.value)",
                ),
                "(0.0002, 0.0002, 0.0002, 0.0002)",
            );
            // SAFETY: the interpreter is up and this thread owns the GIL.
            crate::math::tick(0.5);
            assert_eq!(
                probe(
                    interpreter,
                    "import Math\n__probe = tuple(round(c, 5) for c in morph.value)"
                ),
                "(1.0002, 1.0002, 1.0002, 1.0002)",
            );
        });
    }

    /// The vector types carry their whole script surface: the constructor
    /// forms and their wall, the ostream string, the arithmetic slots with
    /// the Vector3 cross-product multiply, the lexicographic comparisons,
    /// the in-place operators handing the same object back, the method
    /// set with its per-dimension refusals, the attributes, the sequence
    /// protocol, and the bytes state round trip.
    #[test]
    #[ignore = "runs in its own process: see SCENARIOS"]
    fn scenario_the_vector_types_exercise_their_surface() {
        interpreter_scenario(&InitConfig::default(), |interpreter| {
            // Construction: empty is zeros, a single float fills, a tuple
            // sets, and a wrong shape is the set wall.
            assert_eq!(
                probe(
                    interpreter,
                    "import Math\n\
                     __probe = (str(Math.Vector2()), str(Math.Vector2(3)), \
                     str(Math.Vector3(1, 2, 3)), str(Math.Vector4(1, 2, 3, 4)))",
                ),
                "('(0, 0)', '(3, 3)', '(1, 2, 3)', '(1, 2, 3, 4)')",
            );
            assert_eq!(
                probe(
                    interpreter,
                    "import Math\n\
                     try:\n\
                     \x20   Math.Vector2((1, 2, 3))\n\
                     except TypeError as error:\n\
                     \x20   __probe = str(error)",
                ),
                "Vector.set must be set to a sequence of 2 floats, or one float",
            );

            // The arithmetic slots: tuple operands work on either side,
            // scalars multiply and divide, and a fresh vector comes back
            // from every operator.
            assert_eq!(
                probe(
                    interpreter,
                    "import Math\n\
                     v = Math.Vector2((1, 2))\n\
                     w = Math.Vector2((3, 4))\n\
                     __probe = (str(v + w), str(v - w), str(v + (1, 1)), \
                     str((1, 1) + v), str(v * 2), str(2 * v), str(v / 2))",
                ),
                "('(4, 6)', '(-2, -2)', '(2, 3)', '(2, 3)', '(2, 4)', '(2, 4)', '(0.5, 1)')",
            );
            // A division by zero is the ostream's word, not a crash.
            assert_eq!(
                probe(
                    interpreter,
                    "import Math\n__probe = str(Math.Vector2((1, 2)) / 0)",
                ),
                "(inf, inf)",
            );
            assert_eq!(
                probe(
                    interpreter,
                    "import Math\n\
                     v = Math.Vector2((1, 2))\n\
                     __probe = (str(-v), str(+v), bool(Math.Vector2()), bool(v))",
                ),
                "('(-1, -2)', '(1, 2)', False, True)",
            );
            // The Vector3 multiply: two vectors are the cross product, a
            // scalar the plain multiply, anything else the interpreter's
            // own refusal.
            assert_eq!(
                probe(
                    interpreter,
                    "import Math\n\
                     __probe = (str(Math.Vector3((1, 0, 0)) * Math.Vector3((0, 1, 0))), \
                     str(Math.Vector3((1, 0, 0)) * 3))",
                ),
                "('(0, 0, 1)', '(3, 0, 0)')",
            );
            assert_eq!(
                probe(
                    interpreter,
                    "import Math\n\
                     try:\n\
                     \x20   Math.Vector3((1, 0, 0)) * 'nope'\n\
                     except TypeError as error:\n\
                     \x20   __probe = str(error)",
                ),
                "can't multiply sequence by non-int of type 'Math.Vector3'",
            );

            // The comparisons are lexicographic over the components.
            assert_eq!(
                probe(
                    interpreter,
                    "import Math\n\
                     a = Math.Vector2((1, 2))\n\
                     b = Math.Vector2((1, 3))\n\
                     __probe = (a == b, a != b, a < b, b <= a, b > a, a >= b, \
                     a == Math.Vector2((1, 2)))",
                ),
                "(False, True, True, False, True, False, True)",
            );

            // The in-place operators hand the same object back.
            assert_eq!(
                probe(
                    interpreter,
                    "import Math\n\
                     q = Math.Vector2((1, 2))\n\
                     r = q\n\
                     q += (1, 1)\n\
                     __probe = (str(q), r is q)",
                ),
                "('(2, 3)', True)",
            );
            assert_eq!(
                probe(
                    interpreter,
                    "import Math\n\
                     a3 = Math.Vector3((1, 0, 0))\n\
                     a3 *= Math.Vector3((0, 1, 0))\n\
                     __probe = str(a3)",
                ),
                "(0, 0, 1)",
            );
            assert_eq!(
                probe(
                    interpreter,
                    "import Math\n\
                     q = Math.Vector2((1, 2))\n\
                     try:\n\
                     \x20   q += 'nope'\n\
                     except TypeError as error:\n\
                     \x20   __probe = (str(error), str(q))",
                ),
                "(\"unsupported operand type(s) for +=: 'Math.Vector2' and 'str'\", '(1, 2)')",
            );

            // The methods: dot, the cross2D specializations and the
            // refusal past them, the flat distances, the pitch/yaw setter
            // and its refusals, the distance pair, scale, normalise, and
            // the tuple and list views.
            assert_eq!(
                probe(
                    interpreter,
                    "import Math\n\
                     __probe = (Math.Vector2((1, 2)).dot((3, 4)), \
                     Math.Vector2((1, 2)).cross2D((3, 4)), \
                     Math.Vector3((1, 2, 3)).cross2D((4, 5, 6)))",
                ),
                "(11.0, -2.0, -6.0)",
            );
            assert_eq!(
                probe(
                    interpreter,
                    "import Math\n\
                     try:\n\
                     \x20   Math.Vector4((1, 2, 3, 4)).cross2D((1, 2, 3, 4))\n\
                     except TypeError as error:\n\
                     \x20   __probe = str(error)",
                ),
                "No cross2D for this type of vector",
            );
            assert_eq!(
                probe(
                    interpreter,
                    "import Math\n\
                     __probe = (Math.Vector3((1, 2, 3)).flatDistTo((4, 6, 0)), \
                     Math.Vector3((1, 2, 3)).flatDistSqrTo((4, 6, 0)), \
                     Math.Vector2((0, 0)).distTo((3, 4)), \
                     Math.Vector2((0, 0)).distSqrTo((3, 4)))",
                ),
                "(4.242640495300293, 18.0, 5.0, 25.0)",
            );
            assert_eq!(
                probe(
                    interpreter,
                    "import Math\n\
                     try:\n\
                     \x20   Math.Vector2((1, 2)).flatDistTo((3, 4))\n\
                     except TypeError as error:\n\
                     \x20   __probe = str(error)",
                ),
                "No flatDistTo for this type of vector",
            );
            assert_eq!(
                probe(
                    interpreter,
                    "import Math\n\
                     p = Math.Vector3((1, 2, 3))\n\
                     answer = p.setPitchYaw(0.0, 1.0)\n\
                     __probe = (answer, round(p.x, 4), round(p.y, 4), round(p.z, 4))",
                ),
                "(None, 0.8415, -0.0, 0.5403)",
            );
            assert_eq!(
                probe(
                    interpreter,
                    "import Math\n\
                     try:\n\
                     \x20   Math.Vector2((1, 2)).setPitchYaw(1, 2)\n\
                     except TypeError as error:\n\
                     \x20   __probe = str(error)",
                ),
                "No setPitchYaw for this type of vector",
            );
            assert_eq!(
                probe(
                    interpreter,
                    "import Math\n\
                     try:\n\
                     \x20   Math.Vector3((1, 2, 3)).setPitchYaw(1)\n\
                     except TypeError as error:\n\
                     \x20   __probe = str(error)",
                ),
                "Vector3.setPitchYaw expects exactly two arguments",
            );
            assert_eq!(
                probe(
                    interpreter,
                    "import Math\n\
                     s = Math.Vector3((1, 2, 3))\n\
                     scaled = s.scale(2)\n\
                     n = Math.Vector3((3, 0, 0))\n\
                     normalised = n.normalise()\n\
                     __probe = (str(s), str(scaled), str(n), normalised, \
                     Math.Vector2((3, 4)).length, Math.Vector2((3, 4)).lengthSquared)",
                ),
                "('(1, 2, 3)', '(2, 4, 6)', '(1, 0, 0)', None, 5.0, 25.0)",
            );
            assert_eq!(
                probe(
                    interpreter,
                    "import Math\n\
                     try:\n\
                     \x20   Math.Vector3((1, 2, 3)).normalise(1)\n\
                     except TypeError as error:\n\
                     \x20   __probe = str(error)",
                ),
                "Vector.normalise takes no arguments (nor does it brook any dissent :)",
            );
            assert_eq!(
                probe(
                    interpreter,
                    "import Math\n\
                     __probe = (Math.Vector3((1, 2, 3)).tuple(), \
                     Math.Vector3((1, 2, 3)).list(), tuple(Math.Vector2((1, 2))), \
                     len(Math.Vector2((1, 2))))",
                ),
                "((1.0, 2.0, 3.0), [1.0, 2.0, 3.0], (1.0, 2.0), 2)",
            );
            assert_eq!(
                probe(
                    interpreter,
                    "import Math\n\
                     v = Math.Vector2((3, 4))\n\
                     __probe = (v.isReference, v.isReadOnly)",
                ),
                "(False, False)",
            );
            // The yaw and pitch views of a Vector3, in degrees.
            assert_eq!(
                probe(
                    interpreter,
                    "import Math\n\
                     v = Math.Vector3((0, 1, 0))\n\
                     __probe = (round(v.yaw, 4), round(v.pitch, 4))",
                ),
                "(0.0, -1.5708)",
            );

            // The sequence protocol: indexing reads and writes, the
            // negative index, and the out-of-range and non-integer walls.
            assert_eq!(
                probe(
                    interpreter,
                    "import Math\n\
                     v = Math.Vector3((1, 2, 3))\n\
                     __probe = (v[0], v[-1], len(v))",
                ),
                "(1.0, 3.0, 3)",
            );
            assert_eq!(
                probe(
                    interpreter,
                    "import Math\n\
                     try:\n\
                     \x20   Math.Vector3((1, 2, 3))[5]\n\
                     except IndexError as error:\n\
                     \x20   __probe = str(error)",
                ),
                "Vector index out of range",
            );
            assert_eq!(
                probe(
                    interpreter,
                    "import Math\n\
                     v = Math.Vector3((1, 2, 3))\n\
                     v[0] = 9\n\
                     v[-1] = 7\n\
                     __probe = str(v)",
                ),
                "(9, 2, 7)",
            );
            assert_eq!(
                probe(
                    interpreter,
                    "import Math\n\
                     try:\n\
                     \x20   Math.Vector3((1, 2, 3))['a']\n\
                     except TypeError:\n\
                     \x20   __probe = 'TypeError'\n\
                     else:\n\
                     \x20   __probe = 'no error'",
                ),
                "TypeError",
            );

            // The state round trip is a bytes blob of the raw vector.
            assert_eq!(
                probe(
                    interpreter,
                    "import Math\n\
                     blob = Math.Vector3((1, 2, 3)).__getstate__()\n\
                     v = Math.Vector3((9, 9, 9))\n\
                     __probe = (type(blob).__name__, len(blob), v.__setstate__(blob), \
                     str(v))",
                ),
                "('bytes', 12, None, '(1, 2, 3)')",
            );
            assert_eq!(
                probe(
                    interpreter,
                    "import Math\n\
                     try:\n\
                     \x20   Math.Vector3((1, 2, 3)).__setstate__(b'xx')\n\
                     except TypeError as error:\n\
                     \x20   __probe = repr(str(error))",
                ),
                "'__setstate__ called with invalid type\\n'",
            );

            // The per-dimension attribute spread: z and w only where the
            // dimension carries them.
            assert_eq!(
                probe(
                    interpreter,
                    "import Math\n\
                     __probe = (hasattr(Math.Vector2((1, 2)), 'z'), \
                     hasattr(Math.Vector3((1, 2, 3)), 'z'), \
                     hasattr(Math.Vector3((1, 2, 3)), 'w'), \
                     hasattr(Math.Vector4((1, 2, 3, 4)), 'w'), \
                     Math.Vector4((1, 2, 3, 4)).w)",
                ),
                "(False, True, False, True, 4.0)",
            );

            // The component setters write through `safeSetVector`, the
            // in-place operators fold a scalar into every component, and
            // `set` takes every accepted form: separate floats, a tuple, a
            // list, another vector, or one float repeated. The component
            // setter's refusal and the non-3-vector `flatDistSqrTo` are
            // walls (`script_math.cpp:1798-1884, 2110-2157`).
            assert_eq!(
                probe(
                    interpreter,
                    "import Math\n\
                     v = Math.Vector3((1, 2, 3))\n\
                     v.x = 10.0\n\
                     v.y = 20.0\n\
                     v.z = 30.0\n\
                     w = Math.Vector4((1, 1, 1, 1))\n\
                     w.w = 4.0\n\
                     v *= 2.0\n\
                     v /= 2.0\n\
                     w *= 3.0\n\
                     w /= 1.5\n\
                     kind = 'none'\n\
                     try:\n\
                     \x20   v.x = 'x'\n\
                     except TypeError as error:\n\
                     \x20   kind = type(error).__name__\n\
                     packed = Math.Vector3((0, 0, 0))\n\
                     packed.set((5, 6, 7))\n\
                     listed = Math.Vector3((0, 0, 0))\n\
                     listed.set([8, 9, 10])\n\
                     repeated = Math.Vector3((0, 0, 0))\n\
                     repeated.set(42)\n\
                     flat = 'none'\n\
                     try:\n\
                     \x20   w.flatDistSqrTo((0, 0, 0), (0, 0, 0))\n\
                     except TypeError as error:\n\
                     \x20   flat = type(error).__name__\n\
                     __probe = (v.x, v.y, v.z, w.w, kind, packed.x, packed.z, \
                     listed.y, repeated.x, flat)",
                ),
                "(10.0, 20.0, 30.0, 8.0, 'TypeError', 5.0, 7.0, 9.0, 42.0, \
                 'TypeError')",
            );

            // The in-place operator answers `NotImplemented` for a
            // non-number, which Python surfaces as the `TypeError`, and
            // `scale` refuses its non-float argument; a tuple crosses over
            // as a vector and the distance pair takes one -- the arms the
            // arithmetic probes leave alone. The `cross2D` read is the
            // XZ-plane product the reference computes
            // (`script_math.cpp:2014`).
            assert_eq!(
                probe(
                    interpreter,
                    "import Math\n\
                     v = Math.Vector3((2, 0, 0))\n\
                     w = Math.Vector3((0, 3, 0))\n\
                     inplace = 'no'\n\
                     try:\n\
                     \x20   v *= 'x'\n\
                     except BaseException as error:\n\
                     \x20   inplace = type(error).__name__\n\
                     scaled = 'no'\n\
                     try:\n\
                     \x20   v.scale('x')\n\
                     except TypeError as error:\n\
                     \x20   scaled = type(error).__name__\n\
                     try:\n\
                     \x20   dot = v.dot((4, 0, 0))\n\
                     except BaseException as error:\n\
                     \x20   dot = type(error).__name__\n\
                     try:\n\
                     \x20   cross = v.cross2D((0, 5, 0))\n\
                     except BaseException as error:\n\
                     \x20   cross = type(error).__name__\n\
                     try:\n\
                     \x20   near = v.distTo((1, 0, 0))\n\
                     except BaseException as error:\n\
                     \x20   near = type(error).__name__\n\
                     try:\n\
                     \x20   flat = v.flatDistTo((2, 0, 0))\n\
                     except BaseException as error:\n\
                     \x20   flat = type(error).__name__\n\
                     try:\n\
                     \x20   squared = v.flatDistSqrTo((2, 1, 0))\n\
                     except BaseException as error:\n\
                     \x20   squared = type(error).__name__\n\
                     __probe = (inplace, scaled, dot, cross, near, flat, squared)",
                ),
                "('TypeError', 'TypeError', 8.0, 0.0, 1.0, 0.0, 0.0)",
            );

            // The walls the arithmetic probes leave alone: `set` from a
            // vector, its short-tuple zero fill and its refusal, the
            // binary operators answering `NotImplemented` for a non-number,
            // the rich-compare fallthrough, the sequence slot, the
            // `tuple`/`list` argument windows, and the distance and
            // cross-product parses refusing a non-vector.
            assert_eq!(
                probe(
                    interpreter,
                    "import Math\n\
                     def wall(body):\n\
                     \x20   try:\n\
                     \x20       body()\n\
                     \x20       return 'no'\n\
                     \x20   except BaseException as error:\n\
                     \x20       return type(error).__name__\n\
                     def put(seq, key, value):\n\
                     \x20   seq[key] = value\n\
                     v = Math.Vector3((2, 0, 0))\n\
                     s = Math.Vector3((0, 0, 0))\n\
                     short = Math.Vector3((0, 0, 0))\n\
                     sx = 'err'\n\
                     try:\n\
                     \x20   s.set(v)\n\
                     \x20   sx = s.x\n\
                     except BaseException as error:\n\
                     \x20   sx = type(error).__name__\n\
                     shx = 'err'\n\
                     try:\n\
                     \x20   short.set((1, 2))\n\
                     \x20   shx = (short.x, short.z)\n\
                     except BaseException as error:\n\
                     \x20   shx = type(error).__name__\n\
                     sub = 'err'\n\
                     try:\n\
                     \x20   d = Math.Vector4((3, 1, 1, 1)) - Math.Vector4((1, 1, 1, 1))\n\
                     \x20   sub = d.w\n\
                     except BaseException as error:\n\
                     \x20   sub = type(error).__name__\n\
                     bad = wall(lambda: s.set('x'))\n\
                     add = wall(lambda: v + 'x')\n\
                     radd = wall(lambda: 'x' + v)\n\
                     cmp = wall(lambda: v < 'x')\n\
                     assign = wall(lambda: put(v, 0, 5.0))\n\
                     element = wall(lambda: v[0])\n\
                     idx = wall(lambda: put(v, 9, 1.0))\n\
                     typ = wall(lambda: put(v, 0, 'x'))\n\
                     tp = wall(lambda: v.tuple(1))\n\
                     ls = wall(lambda: v.list(1))\n\
                     dt = wall(lambda: v.distTo('x'))\n\
                     ds = wall(lambda: v.distSqrTo('x'))\n\
                     fd = wall(lambda: v.flatDistTo('x'))\n\
                     fs = wall(lambda: v.flatDistSqrTo('x'))\n\
                     c2 = wall(lambda: v.cross2D('x'))\n\
                     __probe = (sx, shx, sub, bad, add, radd, cmp, assign, \
                     element, idx, typ, tp, ls, dt, ds, fd, fs, c2)",
                ),
                "(2.0, 'TypeError', 0.0, 'TypeError', 'TypeError', 'TypeError', \
                 'TypeError', 'no', 'no', 'IndexError', 'TypeError', 'TypeError', \
                 'TypeError', 'TypeError', 'TypeError', 'TypeError', 'TypeError', \
                 'TypeError')",
            );
        });
    }

    /// The matrix and plane types carry their whole script surface, and
    /// the matrix providers read through live while a `Math.Matrix`
    /// constructor copy snapshots: the cell and attribute reads, the
    /// rotation and multiplication order, the inversion and its
    /// singular refusal, the projections, the state blob, the plane ray,
    /// the adaptor's enum wall, and the product and inverse providers.
    #[test]
    #[ignore = "runs in its own process: see SCENARIOS"]
    fn scenario_the_matrix_and_plane_types_exercise_their_surface() {
        interpreter_scenario(&InitConfig::default(), |interpreter| {
            // The cells: identity start, setElement and get, the
            // translation and the axis and origin applications.
            assert_eq!(
                probe(
                    interpreter,
                    "import Math\n\
                     m = Math.Matrix()\n\
                     m.setElement(1, 2, 7)\n\
                     __probe = (m.get(0, 0), m.get(1, 2))",
                ),
                "(1.0, 7.0)",
            );
            assert_eq!(
                probe(
                    interpreter,
                    "import Math\n\
                     m = Math.Matrix()\n\
                     m.setTranslate((3, 4, 5))\n\
                     a = str(m.applyToOrigin())\n\
                     b = str(m.applyToAxis(0))\n\
                     c = str(m.applyV4Point((1, 2, 3, 1)))\n\
                     __probe = (a, b, c)",
                ),
                "('(3, 4, 5)', '(1, 0, 0)', '(4, 6, 8, 1)')",
            );

            // The quarter turn and the vector application: the row
            // layout sends +X to -Z, with the f32 epsilon on the cosine.
            assert_eq!(
                probe(
                    interpreter,
                    "import Math\n\
                     m = Math.Matrix()\n\
                     m.setRotateY(1.5707964)\n\
                     p = m.applyPoint((1, 0, 0))\n\
                     __probe = tuple(round(c, 6) for c in p)",
                ),
                "(-0.0, 0.0, -1.0)",
            );

            // The Euler attributes read back the composition, and the
            // determinant and mirror read the axes.
            assert_eq!(
                probe(
                    interpreter,
                    "import Math\n\
                     m = Math.Matrix()\n\
                     m.setRotateYPR((0.5, -0.25, 0.125))\n\
                     s = Math.Matrix()\n\
                     s.setScale((2, 3, 4))\n\
                     d = Math.Matrix()\n\
                     d.setScale((-2, 3, 4))\n\
                     __probe = (round(m.yaw, 5), round(m.pitch, 5), round(m.roll, 5), \
                     round(s.determinant, 5), s.isMirrored, round(d.determinant, 5), \
                     d.isMirrored)",
                ),
                "(0.5, -0.25, 0.125, 24.0, False, -24.0, True)",
            );

            // The multiplication order: pre lands the argument on the
            // left, post on the right -- the pre-multiplied rotation
            // swings the translated origin, the post one does not.
            assert_eq!(
                probe(
                    interpreter,
                    "import Math\n\
                     r = Math.Matrix()\n\
                     r.setRotateY(1.5707964)\n\
                     t = Math.Matrix()\n\
                     t.setTranslate((1, 0, 0))\n\
                     left = Math.Matrix()\n\
                     left.postMultiply(t)\n\
                     left.preMultiply(r)\n\
                     right = Math.Matrix()\n\
                     right.postMultiply(t)\n\
                     right.postMultiply(r)\n\
                     lp = left.applyToOrigin()\n\
                     rp = right.applyToOrigin()\n\
                     __probe = (tuple(round(c, 4) for c in lp), \
                     tuple(round(c, 4) for c in rp))",
                ),
                "((1.0, 0.0, 0.0), (-0.0, 0.0, -1.0))",
            );

            // The inversion round-trips and the singular refusal leaves
            // the matrix untouched.
            assert_eq!(
                probe(
                    interpreter,
                    "import Math\n\
                     m = Math.Matrix()\n\
                     m.setRotateY(0.9)\n\
                     m.setTranslate((3, -2, 5))\n\
                     o = Math.Matrix()\n\
                     o.setRotateY(0.9)\n\
                     o.setTranslate((3, -2, 5))\n\
                     m.invert()\n\
                     m.postMultiply(o)\n\
                     home = m.applyToOrigin()\n\
                     s = Math.Matrix()\n\
                     s.setScale((0, 1, 1))\n\
                     try:\n\
                     \x20   s.invert()\n\
                     except ValueError as error:\n\
                     \x20   refusal = str(error)\n\
                     __probe = (tuple(abs(c) < 1e-4 for c in home), refusal, \
                     round(s.get(0, 0), 5))",
                ),
                "((True, True, True), 'Matrix could not be inverted', 0.0)",
            );

            // The projections and the lookAt bottom row.
            assert_eq!(
                probe(
                    interpreter,
                    "import Math\n\
                     m = Math.Matrix()\n\
                     m.orthogonalProjection(4, 2, 1, 101)\n\
                     l = Math.Matrix()\n\
                     l.lookAt((1, 2, 3), (0, 0, 1), (0, 1, 0))\n\
                     __probe = (m.get(0, 0), m.get(1, 1), m.get(3, 3), \
                     round(l.get(3, 0), 5), round(l.get(3, 1), 5), round(l.get(3, 2), 5))",
                ),
                "(0.5, 1.0, 1.0, -1.0, -2.0, -3.0)",
            );

            // The state blob round trip.
            assert_eq!(
                probe(
                    interpreter,
                    "import Math\n\
                     m = Math.Matrix()\n\
                     m.setTranslate((1, 2, 3))\n\
                     blob = m.__getstate__()\n\
                     c = Math.Matrix()\n\
                     c.__setstate__(blob)\n\
                     __probe = (type(blob).__name__, len(blob), str(c.applyToOrigin()))",
                ),
                "('bytes', 64, '(1, 2, 3)')",
            );

            // The plane: three points, the ray hit on the plane, and an
            // in-plane ray answers the source itself.
            assert_eq!(
                probe(
                    interpreter,
                    "import Math\n\
                     plane = Math.Plane()\n\
                     plane.init((0, 0, 5), (1, 0, 5), (0, 1, 5))\n\
                     hit = plane.intersectRay((0, 0, 10), (0, 0, -1))\n\
                     flat = plane.intersectRay((0, 0, 5), (1, 0, 0))\n\
                     __probe = (str(hit), str(flat))",
                ),
                "('(0, 0, 5)', '(0, 0, 5)')",
            );

            // The matrix providers read through live, while a Matrix
            // constructor copy snapshots -- the reference documents the
            // difference (`script_math.cpp:857-858`).
            assert_eq!(
                probe(
                    interpreter,
                    "import Math\n\
                     t = Math.Matrix()\n\
                     t.setTranslate((3, 4, 5))\n\
                     only = Math.MatrixProduct()\n\
                     only.a = t\n\
                     only_view = str(Math.Matrix(only).applyToOrigin())\n\
                     p = Math.MatrixProduct()\n\
                     p.a = t\n\
                     p.b = Math.Matrix()\n\
                     via_a = str(Math.Matrix(p.a).applyToOrigin())\n\
                     snapshot = Math.Matrix(p)\n\
                     before = str(snapshot.applyToOrigin())\n\
                     t.setTranslate((9, 9, 9))\n\
                     fresh = str(Math.Matrix(p).applyToOrigin())\n\
                     frozen = str(snapshot.applyToOrigin())\n\
                     __probe = (only_view, via_a, before, fresh, frozen)",
                ),
                "('(3, 4, 5)', '(3, 4, 5)', '(3, 4, 5)', '(9, 9, 9)', '(3, 4, 5)')",
            );

            // The inverse provider negates the rotation and translation.
            assert_eq!(
                probe(
                    interpreter,
                    "import Math\n\
                     t = Math.Matrix()\n\
                     t.setTranslate((3, 4, 5))\n\
                     inverse = Math.MatrixInverse(t)\n\
                     __probe = Math.Matrix(inverse).applyToOrigin()",
                ),
                "(-3, -4, -5)",
            );

            // The adaptor's enum wall and its look-at composition.
            assert_eq!(
                probe(
                    interpreter,
                    "import Math\n\
                     ad = Math.Vector4MatrixAdaptor()\n\
                     ad.style = 'XYZ_SCALE'\n\
                     name = ad.style\n\
                     try:\n\
                     \x20   ad.style = 'BAD'\n\
                     except ValueError as error:\n\
                     \x20   refusal = str(error)\n\
                     __probe = (name, refusal)",
                ),
                "('XYZ_SCALE', 'style must be set to one of XYZ_SCALE, XY_SCALE, \
                 X_ROTATE, Y_ROTATE, Z_ROTATE, LOOKAT, or LOOKAT_SCALEZ')",
            );
            assert_eq!(
                probe(
                    interpreter,
                    "import Math\n\
                     ad = Math.Vector4MatrixAdaptor()\n\
                     ad.style = 'X_ROTATE'\n\
                     ad.source = (1.5707964, 0, 0, 0)\n\
                     ad.position = (5, 0, 0, 0)\n\
                     m = Math.Matrix(ad)\n\
                     first = tuple(round(c, 5) for c in m.applyToAxis(1))\n\
                     ad.source = (0, 0, 0, 0)\n\
                     fresh = tuple(round(c, 5) for c in Math.Matrix(ad).applyToAxis(1))\n\
                     frozen = tuple(round(c, 5) for c in m.applyToAxis(1))\n\
                     __probe = (first, fresh, frozen)",
                ),
                "((0.0, -0.0, 1.0), (0.0, 1.0, 0.0), (0.0, -0.0, 1.0))",
            );
        });
    }
    /// The `Matrix` and `Plane` methods the type scenario leaves alone:
    /// the setters and the element accessors, the three axis rotations
    /// with their angle window, the multiply order and the singular
    /// refusal, the four applications, the two projections, the state
    /// blob's rejections, the translation accessor, the call slot, and
    /// the plane's ray -- every expectation pinned against
    /// `test_script_math.cpp:296-1026`.
    #[test]
    #[ignore = "runs in its own process: see SCENARIOS"]
    fn scenario_the_matrix_and_plane_methods_exercise_their_surface() {
        interpreter_scenario(&InitConfig::default(), |interpreter| {
            // The zero and identity setters, the scale, and the mirror
            // flag the determinant's sign drives.
            assert_eq!(
                probe(
                    interpreter,
                    "import Math\n\
                     zero = Math.Matrix()\n\
                     zero.setZero()\n\
                     ident = Math.Matrix()\n\
                     ident.setScale((2, 3, 4))\n\
                     ident.setIdentity()\n\
                     scale = Math.Matrix()\n\
                     scale.setScale((2, 3, 4))\n\
                     mirror = Math.Matrix()\n\
                     mirror.setScale((1, 1, -1))\n\
                     __probe = (zero.determinant, zero.get(1, 1), ident.determinant, \
                     ident.get(0, 0), ident.get(0, 1), tuple(scale.get(i, i) for i in range(4)), \
                     scale.determinant, mirror.determinant, mirror.isMirrored)",
                ),
                "(0.0, 0.0, 1.0, 1.0, 0.0, (2.0, 3.0, 4.0, 1.0), 24.0, -1.0, True)",
            );

            // `set` copies a whole matrix through the provider parse, and
            // the translation accessor reads and writes the bottom row
            // while the elements mask their indices with `& 3`.
            assert_eq!(
                probe(
                    interpreter,
                    "import Math\n\
                     m = Math.Matrix()\n\
                     trans = Math.Matrix()\n\
                     trans.setTranslate((1, 2, 3))\n\
                     m.set(trans)\n\
                     copied = m.translation.z\n\
                     el = Math.Matrix()\n\
                     el.setElement(3, 0, 1.0)\n\
                     el.setElement(3, 1, 2.0)\n\
                     el.setElement(3, 2, 4.0)\n\
                     rows = (el.get(3, 0), el.get(3, 1), el.get(3, 2), el.get(3, 3))\n\
                     el.translation = (7, 8, 9)\n\
                     after = (el.translation.x, el.translation.y, el.translation.z, \
                     el.get(3, 0), el.get(3, 1), el.get(3, 2), el.get(3, 3), \
                     el.applyToOrigin().z)\n\
                     wrap = Math.Matrix()\n\
                     wrap.setElement(4, 0, 5.0)\n\
                     __probe = (copied, rows, after, wrap.get(0, 0), wrap.get(4, 0))",
                ),
                "(3.0, (1.0, 2.0, 4.0, 1.0), (7.0, 8.0, 9.0, 7.0, 8.0, 9.0, 1.0, 9.0), \
                 5.0, 5.0)",
            );

            // The three axis rotations, the YPR triple, the angle window
            // at both ends, and the determinant every rotation keeps.
            // The `yaw` read back is the Euler decomposition of the stored
            // matrix, so the legal 100 radian angle wraps into `(-pi, pi]` --
            // `100 - 16*pi` in the f32 arithmetic the reference carries
            // (`script_math.cpp:2417-2439`).
            assert_eq!(
                probe(
                    interpreter,
                    "import Math\n\
                     rx = Math.Matrix()\n\
                     rx.setRotateX(0.5)\n\
                     ry = Math.Matrix()\n\
                     ry.setRotateY(0.5)\n\
                     rz = Math.Matrix()\n\
                     rz.setRotateZ(0.5)\n\
                     ypr = Math.Matrix()\n\
                     ypr.setRotateYPR((0.1, 0.2, 0.3))\n\
                     edge = Math.Matrix()\n\
                     edge.setRotateY(100.0)\n\
                     __probe = (round(rx.pitch, 5), round(rx.yaw, 5), round(ry.yaw, 5), \
                     round(ry.pitch, 5), round(rz.roll, 5), round(rz.yaw, 5), \
                     round(ypr.yaw, 5), round(ypr.pitch, 5), round(ypr.roll, 5), \
                     round(ypr.determinant, 5), round(edge.yaw, 5))",
                ),
                "(0.5, 0.0, 0.5, -0.0, 0.5, 0.0, 0.1, 0.2, 0.3, 1.0, -0.53096)",
            );
            assert_eq!(
                probe(
                    interpreter,
                    "import Math\n\
                     edge = Math.Matrix()\n\
                     def wall(body):\n\
                     \x20   try:\n\
                     \x20       body()\n\
                     \x20       return 'no'\n\
                     \x20   except TypeError as error:\n\
                     \x20       return str(error)\n\
                     __probe = (wall(lambda: edge.setRotateY(100.5)), \
                     wall(lambda: edge.setRotateY(-100.5)), \
                     wall(lambda: edge.setRotateX(1000.0)), \
                     wall(lambda: edge.setRotateZ(1000.0)), \
                     wall(lambda: edge.setRotateYPR((0.0, 0.0, 200.0))), \
                     wall(lambda: edge.setRotateYPR((0.0, 200.0, 0.0))), \
                     wall(lambda: edge.setRotateYPR((200.0, 0.0, 0.0))), \
                     wall(lambda: edge.setRotateY()), \
                     wall(lambda: edge.setRotateYPR((0.0, 0.0))), \
                     wall(lambda: edge.setScale(1)), \
                     wall(lambda: edge.setScale()), \
                     wall(lambda: edge.setZero(1)), \
                     wall(lambda: edge.setIdentity(1)))",
                ),
                "('() argument 1 must be a valid angle', '() argument 1 must be a valid angle', \
                 '() argument 1 must be a valid angle', '() argument 1 must be a valid angle', \
                 '() argument 1 element 2 must be a valid angle', \
                 '() argument 1 element 1 must be a valid angle', \
                 '() argument 1 element 0 must be a valid angle', \
                 'setRotateY() expects 1 argument of type float', \
                 '() argument 1 must be set to a Vector3 or a tuple of 3 floats', \
                 '() argument 1 must be set to a Vector3 or a tuple of 3 floats', \
                 'setScale() expects 1 argument of type Vector3', \
                 'setZero() takes no arguments.', \
                 'setIdentity() takes no arguments.')",
            );

            // The multiply order and the inversion: the pre-multiply
            // swings the old translation, the post one keeps it, the
            // inverse undoes it, and a singular matrix is refused with
            // its cells left as they were.
            assert_eq!(
                probe(
                    interpreter,
                    "import Math\n\
                     half = 3.14159 / 2\n\
                     tr = Math.Matrix()\n\
                     tr.setTranslate((1, 0, 0))\n\
                     ro = Math.Matrix()\n\
                     ro.setRotateY(half)\n\
                     pre = Math.Matrix()\n\
                     pre.set(ro)\n\
                     pre.preMultiply(tr)\n\
                     post = Math.Matrix()\n\
                     post.set(ro)\n\
                     post.postMultiply(tr)\n\
                     inv = Math.Matrix()\n\
                     inv.setTranslate((1, 2, 3))\n\
                     inv.invert()\n\
                     back = Math.Matrix()\n\
                     back.set(inv)\n\
                     back.invert()\n\
                     sing = Math.Matrix()\n\
                     sing.setZero()\n\
                     try:\n\
                     \x20   sing.invert()\n\
                     \x20   refusal = 'no'\n\
                     except ValueError as error:\n\
                     \x20   refusal = str(error)\n\
                     __probe = (round(pre.translation.x, 3), round(pre.translation.z, 3), \
                     round(pre.yaw, 2), round(post.translation.x, 3), \
                     round(post.translation.y, 3), round(inv.translation.x, 3), \
                     round(inv.translation.z, 3), round(back.translation.x, 3), \
                     round(back.translation.y, 3), round(back.translation.z, 3), \
                     round(back.determinant, 3), sing.determinant, refusal)",
                ),
                "(0.0, -1.0, 1.57, 1.0, 0.0, -1.0, -3.0, 1.0, 2.0, 3.0, 1.0, 0.0, \
                 'Matrix could not be inverted')",
            );

            // The four applications: the translation reaches points and
            // origins but not vectors, the fourth component divides, and
            // the axis index wraps with `& 3`.
            assert_eq!(
                probe(
                    interpreter,
                    "import Math\n\
                     m = Math.Matrix()\n\
                     m.setTranslate((2, 4, 8))\n\
                     rot = Math.Matrix()\n\
                     rot.setRotateY(3.14159 / 2)\n\
                     p = m.applyPoint((1, 2, 3))\n\
                     v4 = m.applyV4Point((1, 2, 3, 1))\n\
                     v = m.applyVector((1, 2, 3))\n\
                     o = m.applyToOrigin()\n\
                     __probe = ((p.x, p.y, p.z), (v4.x, v4.z, v4.w), (v.x, v.z), \
                     (o.x, o.z), round(rot.applyToAxis(2).x, 3), \
                     round(rot.applyToAxis(2).z, 3), round(rot.applyToAxis(9).x, 3), \
                     round(rot.applyToAxis(9).y, 3))",
                ),
                "((3.0, 6.0, 11.0), (3.0, 11.0, 1.0), (1.0, 3.0), (2.0, 8.0), 1.0, 0.0, \
                 0.0, 1.0)",
            );

            // `lookAt` builds a view matrix whose translation reads back
            // the negated basis dot products.
            assert_eq!(
                probe(
                    interpreter,
                    "import Math\n\
                     look = Math.Matrix()\n\
                     look.lookAt((0, 0, -10), (0, 0, 10), (0, 1, 0))\n\
                     __probe = (round(look.translation.x, 3), round(look.translation.z, 3))",
                ),
                // `-position.dotProduct(Right)` negates the zero dot product,
                // so the `x` translation is the negative zero the reference
                // writes (`matrix.cpp:726`).
                "(-0.0, 10.0)",
            );

            // The projections fill the documented 4x4 layouts.
            assert_eq!(
                probe(
                    interpreter,
                    "import Math\n\
                     orth = Math.Matrix()\n\
                     orth.orthogonalProjection(2.0, 4.0, 1.0, 10.0)\n\
                     persp = Math.Matrix()\n\
                     persp.perspectiveProjection(3.14159265 / 2, 1.0, 1.0, 11.0)\n\
                     __probe = (round(orth.get(0, 0), 4), round(orth.get(0, 1), 4), \
                     round(orth.get(1, 1), 4), round(orth.get(2, 2), 4), \
                     round(orth.get(3, 3), 4), round(orth.translation.z, 4), \
                     round(persp.get(0, 0), 4), round(persp.get(1, 1), 4), \
                     round(persp.get(2, 2), 4), round(persp.get(3, 2), 4), \
                     round(persp.determinant, 4))",
                ),
                "(1.0, 0.0, 0.5, 0.1111, 1.0, -0.1111, 1.0, 1.0, 1.1, -1.1, 1.1)",
            );

            // The state blob round trips, and each rejected shape leaves
            // the target untouched.
            assert_eq!(
                probe(
                    interpreter,
                    "import Math\n\
                     m = Math.Matrix()\n\
                     m.setTranslate((1, 2, 3))\n\
                     state = m.__getstate__()\n\
                     other = Math.Matrix()\n\
                     other.__setstate__(state)\n\
                     kept = (other.translation.x, other.translation.y, other.translation.z)\n\
                     def wall(body, kind):\n\
                     \x20   try:\n\
                     \x20       body()\n\
                     \x20       return 'no'\n\
                     \x20   except kind as error:\n\
                     \x20       return str(error)\n\
                     shaped = (len(state), isinstance(state, bytes), kept, \
                     wall(lambda: other.__setstate__(state, state), TypeError), \
                     wall(lambda: other.__setstate__(('nope',)), ValueError), \
                     wall(lambda: other.__setstate__((b'short',)), ValueError), \
                     other.translation.x)\n\
                     __probe = shaped",
                ),
                "(64, True, (1.0, 2.0, 3.0), \
                 'Matrix.__setstate__ was given wrong args', \
                 'Matrix.__setstate__ was given bad state', \
                 'Matrix.__setstate__ was given bad state', 1.0)",
            );

            // The call slot is `get`: the same two-index read, the same
            // masked lookup, and the same walls.
            assert_eq!(
                probe(
                    interpreter,
                    "import Math\n\
                     m = Math.Matrix()\n\
                     m.setElement(1, 2, 7.0)\n\
                     m.translation = (5, 6, 7)\n\
                     def wall(body):\n\
                     \x20   try:\n\
                     \x20       body()\n\
                     \x20       return 'no'\n\
                     \x20   except TypeError as error:\n\
                     \x20       return str(error)\n\
                     __probe = (m(1, 2), m(5, 2), wall(lambda: m(1)), \
                     wall(lambda: m('x', 0)), wall(lambda: m(0, 'x')), \
                     wall(lambda: Math.Matrix(42)), wall(lambda: Math.Matrix(1, 2)), \
                     wall(lambda: m.set(42)), wall(lambda: m.set(None)), \
                     wall(lambda: m.set()), wall(lambda: m.get()), \
                     wall(lambda: m.setElement(0, 0)), \
                     wall(lambda: m.setElement(0, 0, 'x')), \
                     wall(lambda: m.applyPoint((1.0, 2.0))), \
                     wall(lambda: m.applyV4Point((1.0, 2.0, 3.0))), \
                     wall(lambda: m.applyVector('x')), \
                     wall(lambda: m.applyToAxis()), wall(lambda: m.applyToOrigin(1)), \
                     wall(lambda: m.lookAt((0.0, 0.0, 0.0))), \
                     wall(lambda: m.orthogonalProjection(1.0)), \
                     wall(lambda: m.perspectiveProjection(1.0, 1.0, 1.0)), \
                     wall(lambda: m.preMultiply(42)), \
                     wall(lambda: m.postMultiply(42)), wall(lambda: m.preMultiply()), \
                     str(m.translation))",
                ),
                "(7.0, 7.0, 'get() expects 2 arguments of types uint, uint', \
                 '() argument 1 must be set to an uint', \
                 '() argument 2 must be set to an uint', \
                 'Math.Matrix(): Expected an optional MatrixProvider', \
                 'Math.Matrix(): Expected an optional MatrixProvider', \
                 '() argument 1 must be set to a MatrixProvider or None', \
                 '() argument 1 cannot be None', 'set() expects 1 argument of type \
                 MatrixProvider', 'get() expects 2 arguments of types uint, uint', \
                 'setElement() expects 3 arguments of types uint, uint, float', \
                 '() argument 3 must be set to a float', \
                 '() argument 1 must be set to a Vector3 or a tuple of 3 floats', \
                 '() argument 1 must be set to a Vector4 or tuple of 4 floats', \
                 '() argument 1 must be set to a Vector3 or a tuple of 3 floats', \
                 'applyToAxis() expects 1 argument of type uint', \
                 'applyToOrigin() takes no arguments.', \
                 'lookAt() expects 3 arguments of types Vector3, Vector3, Vector3', \
                 'orthogonalProjection() expects 4 arguments of types float, float, float, \
                 float', \
                 'perspectiveProjection() expects 4 arguments of types float, float, float, \
                 float', \
                 '() argument 1 must be set to a MatrixProvider or None', \
                 '() argument 1 must be set to a MatrixProvider or None', \
                 'preMultiply() expects 1 argument of type MatrixProvider', \
                 '(5, 6, 7)')",
            );

            // The plane: the ray through the floor, the parallel ray that
            // hands the source back, and the parse walls. The reference's
            // own test (`test_script_math.cpp:1019-1022`) only asserts
            // `TypeError` for each, and its `Plane` factory ignores its
            // argument (`script_math.cpp:5279-5283`), so the two-argument
            // `init` fails the count and the extra argument is accepted.
            assert_eq!(
                probe(
                    interpreter,
                    "import Math\n\
                     floor = Math.Plane()\n\
                     floor.init((0, 0, 0), (1, 0, 0), (0, 0, 1))\n\
                     hit = floor.intersectRay((0, 5, 0), (0, -1, 0))\n\
                     par = floor.intersectRay((0, 5, 0), (1, 0, 0))\n\
                     def wall(body):\n\
                     \x20   try:\n\
                     \x20       body()\n\
                     \x20       return 'no'\n\
                     \x20   except TypeError as error:\n\
                     \x20       return str(error)\n\
                     __probe = (round(hit.x, 3), round(hit.y, 3), round(hit.z, 3), \
                     round(par.x, 3), round(par.y, 3), round(par.z, 3), \
                     wall(lambda: floor.init((0, 1, 0))), \
                     wall(lambda: floor.intersectRay((0, 0, 0))), \
                     wall(lambda: floor.init((0, 0, 0), (1, 0))), \
                     wall(lambda: Math.Plane(5).intersectRay((0, 5, 0), (0, -1, 0)).y))",
                ),
                "(0.0, 0.0, 0.0, 0.0, 5.0, 0.0, \
                 'init() expects 3 arguments of types Vector3, Vector3, Vector3', \
                 'intersectRay() expects 2 arguments of types Vector3, Vector3', \
                 'init() expects 3 arguments of types Vector3, Vector3, Vector3', \
                 'no')",
            );

            // The pickling hooks drive a real round trip through the
            // protocol, the translation setter rewrites the bottom row,
            // and a couple of reads the type scenario does not touch. The
            // reference replaces `sys.path` with the resource paths
            // (`script.cpp:566-571`), so the stdlib `copy` module is not
            // importable and the round trip calls the hooks the way
            // `copy.copy` would.
            assert_eq!(
                probe(
                    interpreter,
                    "import Math\n\
                     m = Math.Matrix()\n\
                     m.setTranslate((1, 2, 3))\n\
                     clone = Math.Matrix()\n\
                     clone.__setstate__(m.__getstate__())\n\
                     clone.translation = (4, 5, 6)\n\
                     __probe = (round(clone.translation.x, 3), round(clone.translation.z, 3), \
                     round(clone.applyToOrigin().y, 3), clone.get(3, 3), \
                     round(m.determinant, 3))",
                ),
                "(4.0, 6.0, 5.0, 1.0, 1.0)",
            );

            // The walls the happy-path probes leave alone: the argument
            // windows on `setRotateYPR`, `invert` and `applyV4Point`, the
            // state blob's length check, the plane and application parses,
            // and the translation setter's non-vector refusal.
            assert_eq!(
                probe(
                    interpreter,
                    "import Math\n\
                     def wall(body):\n\
                     \x20   try:\n\
                     \x20       body()\n\
                     \x20       return 'no'\n\
                     \x20   except BaseException as error:\n\
                     \x20       return type(error).__name__\n\
                     m = Math.Matrix()\n\
                     ypr = wall(lambda: m.setRotateYPR())\n\
                     inv = wall(lambda: m.invert(1))\n\
                     v4 = wall(lambda: m.applyV4Point())\n\
                     st = wall(lambda: m.__setstate__(b'short'))\n\
                     pl = Math.Plane()\n\
                     pi = wall(lambda: pl.init('x'))\n\
                     pr = wall(lambda: pl.intersectRay('x'))\n\
                     ap = wall(lambda: m.applyPoint('x'))\n\
                     pre = wall(lambda: m.preMultiply('x'))\n\
                     tr = wall(lambda: setattr(m, 'translation', 'x'))\n\
                     __probe = (ypr, inv, v4, st, pi, pr, ap, pre, tr)",
                ),
                "('TypeError', 'TypeError', 'TypeError', 'ValueError', 'TypeError', \
                 'TypeError', 'TypeError', 'TypeError', 'TypeError')",
            );
        });
    }

    /// The `Vector4` providers exercise their whole script surface: the
    /// product's source forms and walls, the translation and the distance
    /// over matrix origins, the LFO's four waveforms under the shared
    /// tick, the swizzle and the combiner's seven functions, the
    /// animation's keyframe walk with the clock wrap, the morph's clamped
    /// clock, the adaptor's styles, and the shader's register machine --
    /// every expectation pinned against the reference's own unit tests
    /// (`test_script_math.cpp:1028-1608`).
    #[test]
    #[ignore = "runs in its own process: see SCENARIOS"]
    fn scenario_the_v4_providers_exercise_their_surface() {
        interpreter_scenario(&InitConfig::default(), |interpreter| {
            // The product: no source is the zero vector, either source
            // alone passes through, both multiply componentwise, and a
            // tuple or a `Vector4` both wrap as a `Vector4Basic`.
            assert_eq!(
                probe(
                    interpreter,
                    "import Math\n\
                     none = Math.Vector4Product()\n\
                     lone = Math.Vector4Product()\n\
                     lone.a = (1, 2, 0.5, 0)\n\
                     only_b = Math.Vector4Product()\n\
                     only_b.b = (3, 6, 9, 12)\n\
                     prod = Math.Vector4Product()\n\
                     prod.a = (1, 2, 0.5, 0)\n\
                     prod.b = Math.Vector4(2, 2, 2, 2)\n\
                     __probe = (Math.Vector4Product().a is None, type(prod.a).__name__, \
                     str(prod.a.value), str(none.value), str(lone.value), str(only_b.value), \
                     str(prod.value), isinstance(prod.a.value, Math.Vector4))",
                ),
                "(True, 'Vector4Basic', '(1, 2, 0.5, 0)', '(0, 0, 0, 0)', \
                 '(1, 2, 0.5, 0)', '(3, 6, 9, 12)', '(2, 4, 1, 0)', True)",
            );

            // The product's walls: `value` is read-only, a plain number
            // is no provider, and `None` clears a slot to the other
            // source.
            assert_eq!(
                probe(
                    interpreter,
                    "import Math\n\
                     try:\n\
                     \x20   prod.value = (1, 1, 1, 1)\n\
                     \x20   read_only = 'no'\n\
                     except TypeError as error:\n\
                     \x20   read_only = str(error)\n\
                     try:\n\
                     \x20   prod.a = 42\n\
                     \x20   wall = 'no'\n\
                     except TypeError as error:\n\
                     \x20   wall = str(error)\n\
                     prod.a = None\n\
                     __probe = (read_only, wall, str(prod.value))",
                ),
                "('Sorry, the attribute value in Math.Vector4Product is read-only', \
                 'a must be set to a Vector4Provider or None', '(2, 2, 2, 2)')",
            );

            // The translation reads the source's origin with w = 1, and
            // the distance spreads the gap between the two origins into
            // all four components.
            assert_eq!(
                probe(
                    interpreter,
                    "import Math\n\
                     ma = Math.Matrix()\n\
                     ma.setTranslate((1, 2, 3))\n\
                     mb = Math.Matrix()\n\
                     mb.setTranslate((4, 6, 3))\n\
                     trans = Math.Vector4Translation(ma)\n\
                     trans.source = mb\n\
                     dist = Math.Vector4Distance(ma, mb)\n\
                     __probe = (tuple(trans.value), trans.source is mb, \
                     tuple(round(c, 5) for c in dist.value), dist.a is ma, dist.b is mb)",
                ),
                "((4.0, 6.0, 3.0, 1.0), True, (5.0, 5.0, 5.0, 5.0), True, True)",
            );

            // The translation and distance walls: the argument counts and
            // the matrix-provider converter's sentence.
            assert_eq!(
                probe(
                    interpreter,
                    "import Math\n\
                     try:\n\
                     \x20   Math.Vector4Translation(42)\n\
                     except TypeError as error:\n\
                     \x20   not_provider = str(error)\n\
                     try:\n\
                     \x20   Math.Vector4Translation()\n\
                     except TypeError as error:\n\
                     \x20   missing = str(error)\n\
                     try:\n\
                     \x20   Math.Vector4Distance(ma)\n\
                     except TypeError as error:\n\
                     \x20   one = str(error)\n\
                     try:\n\
                     \x20   trans.source = 42\n\
                     except TypeError as error:\n\
                     \x20   attribute = str(error)\n\
                     __probe = (not_provider, missing, one, attribute)",
                ),
                "('() argument 1 must be set to a MatrixProvider or None', \
                 'Vector4Translation() expects 1 argument of type MatrixProvider', \
                 'Vector4Distance() expects 2 arguments of types MatrixProvider, \
                 MatrixProvider', \
                 'source must be set to a MatrixProvider or None')",
            );

            // The LFO's four waveforms at the clock the reference's unit
            // test pins: the half-amplitude sine at zero, the sawtooth
            // ramp, the triangle each side of the half period, the square
            // low then high, and the flat line a zero period draws.
            assert_eq!(
                probe(
                    interpreter,
                    "import Math\n\
                     sine = Math.Vector4LFO()\n\
                     sine.period = 1.0\n\
                     sine.amplitude = 2.0\n\
                     saw = Math.Vector4LFO()\n\
                     saw.period = 1.0\n\
                     saw.amplitude = 2.0\n\
                     saw.waveform = 'SAWTOOTH'\n\
                     saw.time = 0.25\n\
                     tri = Math.Vector4LFO()\n\
                     tri.period = 1.0\n\
                     tri.amplitude = 2.0\n\
                     tri.waveform = 'TRIANGLE'\n\
                     tri.time = 0.25\n\
                     tri_high = Math.Vector4LFO()\n\
                     tri_high.period = 1.0\n\
                     tri_high.amplitude = 2.0\n\
                     tri_high.waveform = 'TRIANGLE'\n\
                     tri_high.time = 0.75\n\
                     sq = Math.Vector4LFO()\n\
                     sq.period = 1.0\n\
                     sq.amplitude = 2.0\n\
                     sq.waveform = 'SQUARE'\n\
                     sq.time = 0.25\n\
                     sq_high = Math.Vector4LFO()\n\
                     sq_high.period = 1.0\n\
                     sq_high.amplitude = 2.0\n\
                     sq_high.waveform = 'SQUARE'\n\
                     sq_high.time = 0.75\n\
                     flat = Math.Vector4LFO()\n\
                     flat.period = 0.0\n\
                     flat.amplitude = 5.0\n\
                     neg = Math.Vector4LFO()\n\
                     neg.period = -1.0\n\
                     neg.amplitude = 5.0\n\
                     neg.waveform = 'SQUARE'\n\
                     neg.time = 0.3\n\
                     __probe = (sine.waveform, round(sine.period, 5), \
                     round(sine.amplitude, 5), round(sine.phase, 5), round(sine.time, 5), \
                     round(sine.value.x, 5), round(saw.value.x, 5), round(tri.value.x, 5), \
                     round(tri_high.value.x, 5), round(sq.value.x, 5), \
                     round(sq_high.value.x, 5), round(flat.value.x, 5), \
                     round(neg.value.x, 5))",
                ),
                "('SINE', 1.0, 2.0, 0.0, 0.0, 1.0, 0.5, 1.5, 1.5, 0.0, 2.0, 0.0, 0.0)",
            );

            // The tick advances the clock and flips the parity: the
            // negative-period square follows the parity counter, which
            // starts out even and goes odd after one frame.
            // SAFETY: the interpreter is up and this thread owns the GIL.
            crate::math::tick(0.1);
            assert_eq!(
                probe(
                    interpreter,
                    "import Math\n__probe = (round(neg.value.x, 5), round(neg.time, 5))",
                ),
                "(5.0, 0.4)",
            );

            // The LFO's walls: the enum sentences and the float parse.
            assert_eq!(
                probe(
                    interpreter,
                    "import Math\n\
                     try:\n\
                     \x20   neg.waveform = 'NOPE'\n\
                     except ValueError as error:\n\
                     \x20   wrong = str(error)\n\
                     try:\n\
                     \x20   neg.waveform = 5\n\
                     except TypeError as error:\n\
                     \x20   not_string = str(error)\n\
                     try:\n\
                     \x20   neg.period = 'x'\n\
                     except TypeError as error:\n\
                     \x20   period = str(error)\n\
                     try:\n\
                     \x20   Math.Vector4LFO(5)\n\
                     except TypeError as error:\n\
                     \x20   factory = str(error)\n\
                     __probe = (wrong, not_string, period, factory)",
                ),
                "('waveform must be set to one of SQUARE, TRIANGLE, SINE, or SAWTOOTH', \
                 'waveform must be set to one of SQUARE, TRIANGLE, SINE, or SAWTOOTH', \
                 'period must be set to a float', \
                 'Vector4LFO() takes no arguments.')",
            );

            // The swizzle gathers the x component of each source into
            // its own slot.
            assert_eq!(
                probe(
                    interpreter,
                    "import Math\n\
                     sw = Math.Vector4Swizzle()\n\
                     sw.x = (1, 9, 9, 9)\n\
                     sw.y = (2, 9, 9, 9)\n\
                     sw.z = (3, 9, 9, 9)\n\
                     sw.w = (4, 9, 9, 9)\n\
                     __probe = (type(sw.x).__name__, tuple(sw.value))",
                ),
                "('Vector4Basic', (1.0, 2.0, 3.0, 4.0))",
            );
            assert_eq!(
                probe(
                    interpreter,
                    "import Math\n\
                     try:\n\
                     \x20   sw.x = 42\n\
                     except TypeError as error:\n\
                     \x20   first = str(error)\n\
                     try:\n\
                     \x20   Math.Vector4Swizzle(5)\n\
                     except TypeError as error:\n\
                     \x20   second = str(error)\n\
                     __probe = (first, second)",
                ),
                "('x must be set to a Vector4Provider or None', \
                 'Vector4Swizzle() takes no arguments.')",
            );

            // The combiner's seven functions over the same two inputs,
            // and the zero vector an absent operand leaves.
            assert_eq!(
                probe(
                    interpreter,
                    "import Math\n\
                     cb = Math.Vector4Combiner()\n\
                     cb.a = (1, 2, 3, 4)\n\
                     cb.b = (4, 3, 2, 1)\n\
                     name = cb.fn\n\
                     add = tuple(round(c, 5) for c in cb.value)\n\
                     cb.fn = 'SUBTRACT'\n\
                     subtract = tuple(round(c, 5) for c in cb.value)\n\
                     cb.fn = 'MULTIPLY'\n\
                     multiply = tuple(round(c, 5) for c in cb.value)\n\
                     cb.fn = 'DIVIDE'\n\
                     divide = tuple(round(c, 5) for c in cb.value)\n\
                     cb.fn = 'DOT'\n\
                     dot = tuple(round(c, 5) for c in cb.value)\n\
                     cb.fn = 'MIN'\n\
                     minimum = tuple(round(c, 5) for c in cb.value)\n\
                     cb.fn = 'MAX'\n\
                     maximum = tuple(round(c, 5) for c in cb.value)\n\
                     cb.a = None\n\
                     zero = tuple(round(c, 5) for c in cb.value)\n\
                     __probe = (name, add, subtract, multiply, divide, dot, minimum, \
                     maximum, zero)",
                ),
                "('ADD', (5.0, 5.0, 5.0, 5.0), (-3.0, -1.0, 1.0, 3.0), (4.0, 6.0, 6.0, 4.0), \
                 (0.25, 0.66667, 1.5, 4.0), (20.0, 20.0, 20.0, 20.0), (1.0, 2.0, 2.0, 1.0), \
                 (4.0, 3.0, 3.0, 4.0), (0.0, 0.0, 0.0, 0.0))",
            );
            assert_eq!(
                probe(
                    interpreter,
                    "import Math\n\
                     try:\n\
                     \x20   cb.fn = 'NOPE'\n\
                     except ValueError as error:\n\
                     \x20   wrong = str(error)\n\
                     try:\n\
                     \x20   cb.fn = 5\n\
                     except TypeError as error:\n\
                     \x20   not_string = str(error)\n\
                     try:\n\
                     \x20   Math.Vector4Combiner(5)\n\
                     except TypeError as error:\n\
                     \x20   factory = str(error)\n\
                     __probe = (wrong, not_string, factory)",
                ),
                "('fn must be set to one of MULTIPLY, DIVIDE, ADD, SUBTRACT, DOT, MIN, or \
                 MAX', \
                 'fn must be set to one of MULTIPLY, DIVIDE, ADD, SUBTRACT, DOT, MIN, or \
                 MAX', \
                 'Vector4Combiner() takes no arguments.')",
            );

            // The animation: empty is the zero vector, the keyframe walk
            // holds the first value before the first keyframe, lerps
            // between, and holds the last past the end. The raw tuple
            // form of `keyframes` reads back the same list shape.
            assert_eq!(
                probe(
                    interpreter,
                    "import Math\n\
                     empty = Math.Vector4Animation()\n\
                     anim = Math.Vector4Animation()\n\
                     anim.duration = 2.0\n\
                     anim.keyframes = [(0.0, (1, 0, 0, 0)), (1.0, (0, 1, 0, 0))]\n\
                     before = tuple(anim.value)\n\
                     anim.time = 0.5\n\
                     half = tuple(anim.value)\n\
                     anim.time = 5.0\n\
                     after = tuple(anim.value)\n\
                     anim.keyframes = ((0.0, (1, 0, 0, 0)), (1.0, (0, 1, 0, 0)))\n\
                     start = Math.Vector4Animation()\n\
                     start.keyframes = [(0.0, (1, 0, 0, 0)), (1.0, (0, 1, 0, 0))]\n\
                     start.time = 0.0\n\
                     __probe = (str(empty.value), round(empty.duration, 5), \
                     len(anim.keyframes), anim.keyframes[0][0], \
                     type(anim.keyframes[0][1]).__name__, before, half, after)",
                ),
                "('(0, 0, 0, 0)', 1.0, 2, 0.0, 'Vector4Basic', \
                 (1.0, 0.0, 0.0, 0.0), (0.5, 0.5, 0.0, 0.0), (0.0, 1.0, 0.0, 0.0))",
            );

            // The shared provider store advances the animation clock and
            // wraps it at the duration.
            // SAFETY: the interpreter is up and this thread owns the GIL.
            crate::math::tick(0.25);
            assert_eq!(
                probe(
                    interpreter,
                    "import Math\n__probe = (round(start.time, 5), round(start.value.x, 5))",
                ),
                "(0.25, 0.75)",
            );
            // SAFETY: the interpreter is up and this thread owns the GIL.
            crate::math::tick(2.0);
            assert_eq!(
                probe(interpreter, "import Math\n__probe = round(start.time, 5)"),
                "0.25",
            );

            // The keyframe wall: each item is a float and a provider, and
            // the assignment itself a sequence. Every bad item -- a short
            // tuple, a non-float clock, a `None` source, a non-numeric
            // clock -- reads as the pair sentence, never the inner wall.
            assert_eq!(
                probe(
                    interpreter,
                    "import Math\n\
                     try:\n\
                     \x20   anim.keyframes = [(0.0, 5)]\n\
                     except TypeError as error:\n\
                     \x20   item = str(error)\n\
                     try:\n\
                     \x20   anim.keyframes = [5]\n\
                     except TypeError as error:\n\
                     \x20   shape = str(error)\n\
                     try:\n\
                     \x20   anim.keyframes = 5\n\
                     except TypeError as error:\n\
                     \x20   sequence = str(error)\n\
                     try:\n\
                     \x20   anim.keyframes = [(0.0, None)]\n\
                     except TypeError as error:\n\
                     \x20   null = str(error)\n\
                     try:\n\
                     \x20   anim.keyframes = [('x', (1, 1, 1, 1))]\n\
                     except TypeError as error:\n\
                     \x20   number = str(error)\n\
                     try:\n\
                     \x20   Math.Vector4Animation(5)\n\
                     except TypeError as error:\n\
                     \x20   factory = str(error)\n\
                     __probe = (item, shape, sequence, null, number, factory)",
                ),
                "('An item of a PySTLSequence of a keyframe must be set to a tuple of a \
                 float and a Vector4Provider', \
                 'An item of a PySTLSequence of a keyframe must be set to a tuple of a \
                 float and a Vector4Provider', \
                 'keyframes must be set to a sequence', \
                 'An item of a PySTLSequence of a keyframe must be set to a tuple of a \
                 float and a Vector4Provider', \
                 'An item of a PySTLSequence of a keyframe must be set to a tuple of a \
                 float and a Vector4Provider', \
                 'Vector4Animation() takes no arguments.')",
            );

            // The morph: the duration rewrite carries the progress and
            // clamps through the 0.0001 floor, and `target` snapshots the
            // current value and rewinds the clock.
            assert_eq!(
                probe(
                    interpreter,
                    "import Math\n\
                     morph = Math.Vector4Morph((0, 0, 0, 0))\n\
                     morph.target = (10, 10, 10, 10)\n\
                     morph.duration = 1.0\n\
                     began = (round(morph.duration, 5), round(morph.time, 5))\n\
                     near = tuple(round(c, 4) for c in morph.value)\n\
                     morph.time = 0.5\n\
                     half = tuple(round(c, 5) for c in morph.value)\n\
                     morph.duration = 2.0\n\
                     carried = (round(morph.duration, 5), round(morph.time, 5), \
                     tuple(round(c, 5) for c in morph.value))\n\
                     morph.time = 100.0\n\
                     clamped = (round(morph.time, 5), tuple(round(c, 5) for c in morph.value))\n\
                     morph.time = -5.0\n\
                     floor = round(morph.time, 5)\n\
                     morph.duration = 0.0\n\
                     duration_floor = round(morph.duration, 5)\n\
                     morph.time = 1.0\n\
                     morph.target = (20, 20, 20, 20)\n\
                     retarget = (round(morph.time, 5), round(morph.target.x, 5))\n\
                     __probe = (began, near, half, carried, clamped, floor, duration_floor, \
                     retarget)",
                ),
                "((1.0, 0.0001), (0.001, 0.001, 0.001, 0.001), (5.0, 5.0, 5.0, 5.0), \
                 (2.0, 1.0, (5.0, 5.0, 5.0, 5.0)), (2.0, (10.0, 10.0, 10.0, 10.0)), \
                 0.0001, 0.0001, (0.0, 20.0))",
            );
            assert_eq!(
                probe(
                    interpreter,
                    "import Math\n\
                     try:\n\
                     \x20   Math.Vector4Morph(5)\n\
                     except TypeError as error:\n\
                     \x20   first = str(error)\n\
                     try:\n\
                     \x20   morph.target = 5\n\
                     except TypeError as error:\n\
                     \x20   second = str(error)\n\
                     try:\n\
                     \x20   Math.Vector4Morph(1, 2)\n\
                     except TypeError as error:\n\
                     \x20   third = str(error)\n\
                     __probe = (first, second, third)",
                ),
                "('() argument 1 optionally must be set to a Vector4 or tuple of 4 floats', \
                 'target must be set to a Vector4 or tuple of 4 floats', \
                 'Vector4Morph() expects between 0 and 1 arguments of types Vector4')",
            );

            // The adaptor's styles: the scales read the components, the
            // rotations their angles, the look-ats stay invertible, and a
            // missing source leaves the receiving matrix zeroed.
            assert_eq!(
                probe(
                    interpreter,
                    "import Math\n\
                     ad = Math.Vector4MatrixAdaptor()\n\
                     ad.source = (2, 3, 4, 0)\n\
                     ad.position = (1, 0, 0, 0)\n\
                     xy = (round(Math.Matrix(ad).get(0, 0), 5), \
                     round(Math.Matrix(ad).get(1, 1), 5), \
                     round(Math.Matrix(ad).get(2, 2), 5), \
                     round(Math.Matrix(ad).translation.x, 5))\n\
                     xyz = Math.Vector4MatrixAdaptor()\n\
                     xyz.source = (2, 3, 4, 0)\n\
                     xyz.style = 'XYZ_SCALE'\n\
                     rot_y = Math.Vector4MatrixAdaptor()\n\
                     rot_y.source = (2, 3, 4, 0)\n\
                     rot_y.style = 'Y_ROTATE'\n\
                     rot_z = Math.Vector4MatrixAdaptor()\n\
                     rot_z.source = (2, 3, 4, 0)\n\
                     rot_z.style = 'Z_ROTATE'\n\
                     look = Math.Vector4MatrixAdaptor()\n\
                     look.source = (1, 0, 0, 0)\n\
                     look.style = 'LOOKAT'\n\
                     look_z = Math.Vector4MatrixAdaptor()\n\
                     look_z.source = (0, 0, 2, 0)\n\
                     look_z.style = 'LOOKAT_SCALEZ'\n\
                     __probe = (xy, round(Math.Matrix(xyz).get(2, 2), 5), \
                     round(Math.Matrix(rot_y).yaw, 5), round(Math.Matrix(rot_z).roll, 5), \
                     Math.Matrix(look).determinant != 0, \
                     Math.Matrix(look_z).determinant != 0, \
                     round(Math.Matrix(Math.Vector4MatrixAdaptor()).determinant, 5), \
                     round(Math.Matrix(Math.Vector4MatrixAdaptor()).translation.x, 5))",
                ),
                "((2.0, 3.0, 1.0, 1.0), 4.0, 3.0, -2.28319, True, True, 0.0, 0.0)",
            );
            assert_eq!(
                probe(
                    interpreter,
                    "import Math\n\
                     try:\n\
                     \x20   Math.Vector4MatrixAdaptor(5)\n\
                     except TypeError as error:\n\
                     \x20   factory = str(error)\n\
                     try:\n\
                     \x20   ad.source = 42\n\
                     except TypeError as error:\n\
                     \x20   source = str(error)\n\
                     try:\n\
                     \x20   ad.position = 42\n\
                     except TypeError as error:\n\
                     \x20   position = str(error)\n\
                     __probe = (factory, source, position)",
                ),
                "('Vector4MatrixAdaptor() takes no arguments.', \
                 'source must be set to a Vector4Provider or None', \
                 'position must be set to a Vector4Provider or None')",
            );

            // The register file: one shared handle per cell, zeros until
            // a shader writes, and the scratch cap's `MAX_ARG` sentence.
            assert_eq!(
                probe(
                    interpreter,
                    "import Math\n\
                     reg = Math.getRegister(0)\n\
                     last = Math.getRegister(62)\n\
                     try:\n\
                     \x20   Math.getRegister(63)\n\
                     except TypeError as error:\n\
                     \x20   over = str(error)\n\
                     try:\n\
                     \x20   Math.getRegister()\n\
                     except TypeError as error:\n\
                     \x20   count = str(error)\n\
                     try:\n\
                     \x20   Math.getRegister('x')\n\
                     except TypeError as error:\n\
                     \x20   parse = str(error)\n\
                     __probe = (Math.getRegister(0) is reg, tuple(reg.value), over, count, \
                     parse)",
                ),
                "(True, (0.0, 0.0, 0.0, 0.0), \
                 '() argument 1 is greater than maximum value', \
                 'getRegister() expects 1 argument of type uint8', \
                 '() argument 1 must be set to an uint')",
            );

            // The one-operand shader ops over register zero.
            assert_eq!(
                probe(
                    interpreter,
                    "import Math\n\
                     def const(x, y, z, w):\n\
                     \x20   p = Math.Vector4Product()\n\
                     \x20   p.a = (x, y, z, w)\n\
                     \x20   return p\n\
                     move = Math.Vector4Shader()\n\
                     move.addOp(0, reg, const(5, 6, 7, 8))\n\
                     recip = Math.Vector4Shader()\n\
                     recip.addOp(1, reg, const(2, 4, 8, 16))\n\
                     bias = Math.Vector4Shader()\n\
                     bias.addOp(2, reg, const(1, 2, 3, 4))\n\
                     complement = Math.Vector4Shader()\n\
                     complement.addOp(3, reg, const(1, 2, 3, 4))\n\
                     __probe = (tuple(round(c, 5) for c in move.value), \
                     tuple(round(c, 5) for c in recip.value), \
                     tuple(round(c, 5) for c in bias.value), \
                     tuple(round(c, 5) for c in complement.value))",
                ),
                "((5.0, 6.0, 7.0, 8.0), (0.5, 0.25, 0.125, 0.0625), \
                 (0.5, 1.5, 2.5, 3.5), (0.0, -1.0, -2.0, -3.0))",
            );

            // The two-operand ops, the comparisons included: the
            // staircase against i0.x the reference pins.
            assert_eq!(
                probe(
                    interpreter,
                    "import Math\n\
                     mul = Math.Vector4Shader()\n\
                     mul.addOp(4, reg, const(1, 2, 3, 4), const(10, 20, 30, 40))\n\
                     div = Math.Vector4Shader()\n\
                     div.addOp(5, reg, const(1, 2, 3, 4), const(2, 2, 2, 2))\n\
                     add = Math.Vector4Shader()\n\
                     add.addOp(6, reg, const(1, 2, 3, 4), const(10, 20, 30, 40))\n\
                     sub = Math.Vector4Shader()\n\
                     sub.addOp(7, reg, const(1, 2, 3, 4), const(1, 1, 1, 1))\n\
                     dot = Math.Vector4Shader()\n\
                     dot.addOp(8, reg, const(1, 2, 3, 4), const(4, 3, 2, 1))\n\
                     mn = Math.Vector4Shader()\n\
                     mn.addOp(9, reg, const(1, 2, 3, 4), const(4, 3, 2, 1))\n\
                     mx = Math.Vector4Shader()\n\
                     mx.addOp(10, reg, const(1, 2, 3, 4), const(4, 3, 2, 1))\n\
                     greater = Math.Vector4Shader()\n\
                     greater.addOp(11, reg, const(1, 2, 3, 4), const(4, 3, 2, 1))\n\
                     less = Math.Vector4Shader()\n\
                     less.addOp(12, reg, const(1, 2, 3, 4), const(4, 3, 2, 1))\n\
                     __probe = (tuple(round(c, 5) for c in mul.value), \
                     tuple(round(c, 5) for c in div.value), \
                     tuple(round(c, 5) for c in add.value), \
                     tuple(round(c, 5) for c in sub.value), \
                     tuple(round(c, 5) for c in dot.value), \
                     tuple(round(c, 5) for c in mn.value), \
                     tuple(round(c, 5) for c in mx.value), \
                     tuple(round(c, 5) for c in greater.value), \
                     tuple(round(c, 5) for c in less.value))",
                ),
                "((10.0, 40.0, 90.0, 160.0), (0.5, 1.0, 1.5, 2.0), (11.0, 22.0, 33.0, 44.0), \
                 (0.0, 1.0, 2.0, 3.0), (20.0, 20.0, 20.0, 20.0), (1.0, 2.0, 2.0, 1.0), \
                 (4.0, 3.0, 3.0, 4.0), (0.0, 0.0, 0.0, 1.0), (1.0, 1.0, 1.0, 0.0))",
            );

            // The instructions run in order with the last write to
            // register zero winning, a register an earlier shader wrote
            // feeds a later one as a plain input, and the same provider in
            // both operand slots is one user input.
            assert_eq!(
                probe(
                    interpreter,
                    "import Math\n\
                     ordered = Math.Vector4Shader()\n\
                     ordered.addOp(6, reg, const(1, 1, 1, 1), const(1, 1, 1, 1))\n\
                     ordered.addOp(4, reg, const(1, 2, 3, 4), const(10, 20, 30, 40))\n\
                     feeder = Math.Vector4Shader()\n\
                     feeder.addOp(6, last, const(1, 2, 3, 4), const(1, 1, 1, 1))\n\
                     feeder.value\n\
                     reader = Math.Vector4Shader()\n\
                     reader.addOp(0, reg, last)\n\
                     shared = const(3, 3, 3, 3)\n\
                     dedupe = Math.Vector4Shader()\n\
                     dedupe.addOp(6, reg, shared, shared)\n\
                     __probe = (tuple(round(c, 5) for c in ordered.value), \
                     tuple(round(c, 5) for c in reader.value), \
                     tuple(round(c, 5) for c in dedupe.value))",
                ),
                "((10.0, 40.0, 90.0, 160.0), (2.0, 3.0, 4.0, 5.0), (6.0, 6.0, 6.0, 6.0))",
            );

            // A non-register output slot maps to the unused cell, an
            // unknown opcode writes nothing, plain tuples coerce as
            // constant snapshots, and the 65th distinct user input
            // reuses the unused cell rather than a 65th register. Both
            // non-writing shaders answer the shared register file's cell
            // zero: `r_[]` is class-static and only the user inputs at
            // `BASE_USER_REGISTER` upward are ever written
            // (`script_math.cpp:5017,5126-5131`), so the leftover is the
            // `(6, 6, 6, 6)` the previous probe left in it.
            assert_eq!(
                probe(
                    interpreter,
                    "import Math\n\
                     misplaced = Math.Vector4Shader()\n\
                     misplaced.addOp(4, const(1, 1, 1, 1), const(1, 1, 1, 1))\n\
                     unknown = Math.Vector4Shader()\n\
                     unknown.addOp(13, reg, const(7, 8, 9, 10))\n\
                     tuples = Math.Vector4Shader()\n\
                     tuples.addOp(0, reg, (1, 1, 1, 1))\n\
                     tuples.addOp(4, reg, reg, (3, 3, 3, 3))\n\
                     capped = Math.Vector4Shader()\n\
                     for i in range(65):\n\
                     \x20   capped.addOp(4, reg, (i, i, i, i))\n\
                     __probe = (tuple(round(c, 5) for c in misplaced.value), \
                     tuple(round(c, 5) for c in unknown.value), \
                     tuple(round(c, 5) for c in tuples.value), \
                     tuple(round(c, 5) for c in capped.value))",
                ),
                "((6.0, 6.0, 6.0, 6.0), (6.0, 6.0, 6.0, 6.0), (3.0, 3.0, 3.0, 3.0), \
                 (0.0, 0.0, 0.0, 0.0))",
            );

            // The shader's walls: the addOp argument window, the opcode
            // and operand parses, and the factory.
            assert_eq!(
                probe(
                    interpreter,
                    "import Math\n\
                     sh = Math.Vector4Shader()\n\
                     try:\n\
                     \x20   sh.addOp(4)\n\
                     except TypeError as error:\n\
                     \x20   count = str(error)\n\
                     try:\n\
                     \x20   sh.addOp('x', reg, sh)\n\
                     except TypeError as error:\n\
                     \x20   opcode = str(error)\n\
                     try:\n\
                     \x20   sh.addOp(4, 42, sh)\n\
                     except TypeError as error:\n\
                     \x20   output = str(error)\n\
                     try:\n\
                     \x20   sh.addOp(4, reg, Math.Matrix())\n\
                     except TypeError as error:\n\
                     \x20   operand = str(error)\n\
                     try:\n\
                     \x20   Math.Vector4Shader(5)\n\
                     except TypeError as error:\n\
                     \x20   factory = str(error)\n\
                     __probe = (count, opcode, output, operand, factory)",
                ),
                "('addOp() expects between 3 and 4 arguments of types uint8, \
                 Vector4Provider, Vector4Provider, Vector4Provider', \
                 '() argument 1 must be set to an uint', \
                 '() argument 2 must be set to a Vector4Provider or None', \
                 '() argument 3 must be set to a Vector4Provider or None', \
                 'Vector4Shader() takes no arguments.')",
            );

            // The matrix providers' walls and the single-slot form.
            assert_eq!(
                probe(
                    interpreter,
                    "import Math\n\
                     t = Math.Matrix()\n\
                     t.setTranslate((7, 8, 9))\n\
                     only_b = Math.MatrixProduct()\n\
                     only_b.b = t\n\
                     via_b = str(Math.Matrix(only_b).applyToOrigin())\n\
                     try:\n\
                     \x20   Math.MatrixProduct(5)\n\
                     except TypeError as error:\n\
                     \x20   factory = str(error)\n\
                     try:\n\
                     \x20   Math.MatrixInverse()\n\
                     except TypeError as error:\n\
                     \x20   missing = str(error)\n\
                     try:\n\
                     \x20   Math.MatrixInverse(42)\n\
                     except TypeError as error:\n\
                     \x20   not_provider = str(error)\n\
                     __probe = (via_b, factory, missing, not_provider)",
                ),
                "('(7, 8, 9)', 'MatrixProduct() takes no arguments.', \
                 'MatrixInverse() expects 1 argument of type MatrixProvider', \
                 '() argument 1 must be set to a MatrixProvider or None')",
            );

            // The remaining provider walls and lazy slots: the morph clamp
            // setters, both Distance endpoint parses, the swizzle's unset
            // components, the LFO clocks at a dead period, the bare product
            // the matrix folds as identity, and the product slot that a
            // None clears back out.
            assert_eq!(
                probe(
                    interpreter,
                    "import Math\n\
                     def wall(body):\n\
                     \x20   try:\n\
                     \x20       body()\n\
                     \x20       return 'no'\n\
                     \x20   except BaseException as error:\n\
                     \x20       return type(error).__name__\n\
                     mo = Math.Vector4Morph()\n\
                     du = wall(lambda: setattr(mo, 'duration', 'x'))\n\
                     ti = wall(lambda: setattr(mo, 'time', 'x'))\n\
                     d1 = wall(lambda: Math.Vector4Distance('x', Math.Matrix()))\n\
                     d2 = wall(lambda: Math.Vector4Distance(Math.Matrix(), 'x'))\n\
                     sz = str(Math.Vector4Swizzle().value)\n\
                     tri = Math.Vector4LFO()\n\
                     tri.waveform = 'TRIANGLE'\n\
                     tri.period = 0.0\n\
                     tv = tuple(round(c, 5) for c in tri.value)\n\
                     saw = Math.Vector4LFO()\n\
                     saw.waveform = 'SAWTOOTH'\n\
                     saw.period = 0.0\n\
                     wv = tuple(round(c, 5) for c in saw.value)\n\
                     bare = str(Math.Matrix(Math.MatrixProduct()).applyToOrigin())\n\
                     mp = Math.MatrixProduct()\n\
                     mp.a = None\n\
                     mp.b = Math.Matrix()\n\
                     mp.b.setTranslate((7, 8, 9))\n\
                     via_none = str(Math.Matrix(mp).applyToOrigin())\n\
                     __probe = (du, ti, d1, d2, sz, tv, wv, bare, via_none)",
                ),
                "('TypeError', 'TypeError', 'TypeError', 'TypeError', \
                 '(0, 0, 0, 0)', (0.0, 0.0, 0.0, 0.0), \
                 (0.0, 0.0, 0.0, 0.0), '(0, 0, 0)', '(7, 8, 9)')",
            );

            // The leftover walls across the three families: the vector
            // subtraction, the zero-argument construction, the tuple parse
            // over a non-number, the division and two-wide multiplication
            // NotImplemented falls, the register read-only wall, the
            // matrix argument parses, the shader's fourth operand, the
            // inverse with a cleared source, and the look-at over the pole
            // where the reference swaps the up axis.
            assert_eq!(
                probe(
                    interpreter,
                    "import Math\n\
                     def wall(body):\n\
                     \x20   try:\n\
                     \x20       body()\n\
                     \x20       return 'no'\n\
                     \x20   except BaseException as error:\n\
                     \x20       return type(error).__name__\n\
                     sub = str(Math.Vector3((1, 2, 3)) - Math.Vector3((1, 1, 1)))\n\
                     zero = str(Math.Vector3())\n\
                     dnot = wall(lambda: Math.Vector3((1, 2, 3)) / 'x')\n\
                     mnot = wall(lambda: Math.Vector2((1, 0)) * Math.Vector2((0, 1)))\n\
                     badtuple = wall(lambda: Math.Vector3((1, 'x', 3)))\n\
                     dt = wall(lambda: Math.Vector3().dot('x'))\n\
                     c2 = wall(lambda: Math.Vector3().cross2D('x'))\n\
                     sf = wall(lambda: Math.Vector3().set(1, 2, 3))\n\
                     py = wall(lambda: Math.Vector3().setPitchYaw('x', 1))\n\
                     m = Math.Matrix()\n\
                     st = wall(lambda: m.setTranslate('x'))\n\
                     lk = wall(lambda: m.lookAt((0, 0, 0), 'x', (0, 1, 0)))\n\
                     g1 = wall(lambda: m.get('x', 1))\n\
                     g2 = wall(lambda: m.get(1, 'x'))\n\
                     ax = wall(lambda: m.applyToAxis('x'))\n\
                     p = Math.Plane()\n\
                     pi = wall(lambda: p.init((0, 0, 0), 'x', (0, 1, 0)))\n\
                     ir = wall(lambda: p.intersectRay('x', (0, 0, 1)))\n\
                     sh = Math.Vector4Shader()\n\
                     reg = Math.getRegister(3)\n\
                     a4 = wall(lambda: sh.addOp(4, reg, sh, 'x'))\n\
                     vv = Math.Vector3((4, 5, 6))\n\
                     vv -= Math.Vector3((1, 1, 1))\n\
                     insub = str(vv)\n\
                     d0 = round(Math.Vector3((1, 2, 3)).dot(), 5)\n\
                     inv_id = str(Math.Matrix(Math.MatrixInverse(None)).applyToOrigin())\n\
                     look_up = Math.Vector4MatrixAdaptor()\n\
                     look_up.source = (0, 10, 0, 0)\n\
                     look_up.style = 'LOOKAT'\n\
                     look_up_z = Math.Vector4MatrixAdaptor()\n\
                     look_up_z.source = (0, 5, 0, 0)\n\
                     look_up_z.style = 'LOOKAT_SCALEZ'\n\
                     lu = Math.Matrix(look_up).determinant != 0\n\
                     lz = Math.Matrix(look_up_z).determinant != 0\n\
                     __probe = (sub, zero, dnot, mnot, badtuple, dt, c2, sf, py, st, \
                     lk, g1, g2, ax, pi, ir, a4, insub, d0, inv_id, lu, lz)",
                ),
                "('(0, 1, 2)', '(0, 0, 0)', 'TypeError', 'TypeError', 'TypeError', \
                 'TypeError', 'TypeError', 'no', 'TypeError', 'TypeError', \
                 'TypeError', 'TypeError', 'TypeError', 'TypeError', 'TypeError', \
                 'TypeError', 'TypeError', '(3, 4, 5)', 0.0, '(0, 0, 0)', True, True)",
            );
        });
    }
}
