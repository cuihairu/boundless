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
use std::sync::atomic::{AtomicBool, Ordering};

use super::capi::{self, PyObject};
use super::import_paths::ImportPaths;

static INITIALIZED: AtomicBool = AtomicBool::new(false);

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
        let error = if !config.argv.is_empty() && !set_argv(&config.argv) {
            Some(InitError::ArgvFailed)
        } else if !set_import_paths(&config.import_paths) {
            Some(InitError::PathFailed)
        } else if !disable_garbage() {
            Some(InitError::GarbageCollectionUnavailable)
        } else {
            None
        };
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

    /// `PyRun_SimpleString`: the reference returns 0 on success.
    pub fn run_simple_string(&self, source: &str) -> bool {
        let Ok(encoded) = CString::new(source) else {
            return false;
        };
        // SAFETY: the interpreter is up and this thread owns the GIL.
        unsafe { capi::PyRun_SimpleString(encoded.as_ptr()) == 0 }
    }

    /// `Script::fini`: always tears the interpreter down in this port.
    ///
    /// The reference can skip `Py_Finalize` (client-module builds) and still
    /// mark the script system finalised; a server process never does, so the
    /// flag is the whole difference and stays for the shutdown item to
    /// exercise.
    pub fn fini(&self) {
        if !INITIALIZED.load(Ordering::SeqCst) || capi::is_finalized() {
            return;
        }
        // SAFETY: the interpreter is up, this thread owns the GIL, and the
        // steps below run in the reference's order.
        unsafe {
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

/// `Script::disablePythonGarbage`: import `gc` and call `gc.disable()`.
///
/// The reference clears an import failure and carries on with collection
/// enabled; the port reports it instead, see [`InitError`].
///
/// SAFETY: the interpreter must be up and the caller must own the GIL.
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
        let interpreter = Interpreter::init(config).expect("Script::init");
        body(&interpreter);
        interpreter.fini();
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
        root
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
        let interpreter = Interpreter::init(&InitConfig::default()).expect("Script::init");
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
            let _interpreter = Interpreter::init(&InitConfig::default()).expect("Script::init");
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
        let interpreter = Interpreter::init(&InitConfig::default()).expect("Script::init");
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
            },
            |interpreter| {
                assert_eq!(
                    probe(interpreter, "import sys; __probe = len(sys.path)"),
                    "4"
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
            })
            .err(),
            Some(InitError::PathFailed),
        );
        assert!(!Interpreter::is_initialized());
        assert_eq!(unsafe { capi::Py_IsInitialized() }, 0);
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
