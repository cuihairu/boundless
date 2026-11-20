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

use super::capi::{self, PyObject};
use super::import_paths::ImportPaths;
use super::init_time_job;
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
        // The native registries die before the script system finalises --
        // `CellApp::shutDown` deletes its entities before `Script::fini` --
        // so every native wrapper `Py_DecRef`s here, allocator still alive.
        wrapper::reset();
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
    use crate::import_paths;
    use crate::pyref::PyRef;
    use std::path::PathBuf;
    use std::process::Command;

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
        "interpreter::tests::scenario_init_runs_the_init_time_jobs",
        "interpreter::tests::scenario_init_run_fini",
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
            let borrowed = PyRef::borrowed(text);
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
}
