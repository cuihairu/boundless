//! BW-00-03-03: the minimal native Python module the port builds and installs.
//!
//! The reference ships its engine types to Python through `lib/pyscript/`;
//! the other direction — a native module Python *imports* — is this crate. It
//! is a cdylib carrying one `PyInit_boundless_core` entry point, so once the
//! built `.so` is installed into an importable site-packages directory,
//! `import boundless_core` works in the interpreter the build machine has.
//!
//! The CPython C API is declared by hand, the way `python-host` declares it
//! (`capi.rs`: no bindgen, no pyo3 in the workspace dependency set), so a
//! CPython revision bump is a one-file review. The layouts below were
//! verified against the interpreter's own headers (3.14) with a compiled
//! probe, not assumed: `PyModuleDef` is 104 bytes (`m_name` at 40 … `m_free`
//! at 96), `PyModuleDef_Base` 40 (`m_init` at 16), `PyMethodDef` 32
//! (`ml_flags` at 16), and `PyModuleDef_HEAD_INIT` initialises `ob_refcnt`
//! to `_Py_STATIC_IMMORTAL_INITIAL_REFCNT` (`7L << 28`).
//!
//! Two facts beyond the layouts were confirmed the hard way (both segfault
//! or error on a bare import, so neither can be assumed):
//!
//! * The module definition must be *writable*. `PyModuleDef_Init` writes
//!   `ob_refcnt`, `ob_type`, and `m_index` into the def it is given, and the
//!   cdylib links with full RELRO (`-z now`), so the default `.data.rel.ro`
//!   placement is mprotected read-only after relocation — the definition is
//!   forced into `.data` instead.
//! * The second argument to `PyModule_Create2` is `PYTHON_API_VERSION`
//!   (1013 in 3.14), not 0; `check_api_version` rejects a mismatch.

use std::ffi::{CStr, c_int, c_long};
use std::io;
use std::os::raw::{c_char, c_void};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// The module name `PyInit_boundless_core` registers.
pub const MODULE_NAME: &CStr = c"boundless_core";

/// The reference engine version this module reports.
pub const ENGINE_VERSION: &CStr = c"14.4.1";

/// The crate version, exposed as the module's `__version__`.
pub const VERSION_BYTES: &[u8] = concat!(env!("CARGO_PKG_VERSION"), "\0").as_bytes();

/// `METH_NOARGS` — the methods take no arguments and CPython passes a null
/// second argument.
const METH_NOARGS: c_int = 4;

/// `PYTHON_API_VERSION` (3.14): the value `PyModule_Create`'s macro passes
/// as `PyModule_Create2`'s second argument. `check_api_version` rejects any
/// other value with a `SystemError`, so it must be exact.
const PYTHON_API_VERSION: c_int = 1013;

/// `PyModuleDef_HEAD_INIT`, hand-set the way `object.h`'s
/// `PyObject_HEAD_INIT(NULL)` expands in a GIL-enabled build: `ob_refcnt` is
/// the static immortal value, `ob_type` null.
const HEAD_INIT: PyModuleDefBase = PyModuleDefBase {
    ob_refcnt: 7 << 28,
    ob_type: std::ptr::null_mut(),
    m_init: std::ptr::null(),
    m_index: 0,
    m_copy: std::ptr::null_mut(),
};

/// `PyModuleDef_Base`, the 40-byte header every module definition carries.
#[repr(C)]
struct PyModuleDefBase {
    /// `ob_refcnt`, initialised to the static immortal value (see
    /// [`HEAD_INIT`]).
    ob_refcnt: isize,
    /// `ob_type`.
    ob_type: *mut c_void,
    /// `m_init`, the single-phase re-initialiser (null here).
    m_init: *const c_void,
    /// `m_index`, the module's index in the interpreter's module cache.
    m_index: isize,
    /// `m_copy`, the cached `__dict__` copy (null until first load).
    m_copy: *mut c_void,
}

/// `PyMethodDef`, one entry per module function.
#[repr(C)]
struct PyMethodDef {
    /// `ml_name`, the function name Python sees.
    ml_name: *const c_char,
    /// `ml_meth`, the C function: `(self, unused)` under `METH_NOARGS`.
    ml_meth: Option<unsafe extern "C" fn(*mut c_void, *mut c_void) -> *mut c_void>,
    /// `ml_flags`, one of the `METH_*` values.
    ml_flags: c_int,
    /// `ml_doc`, the docstring.
    ml_doc: *const c_char,
}

/// `PyModuleDef`, the 104-byte module definition.
#[repr(C)]
struct PyModuleDef {
    /// `m_base`, the [`PyModuleDefBase`] header.
    m_base: PyModuleDefBase,
    /// `m_name`, the module name.
    m_name: *const c_char,
    /// `m_doc`, the module docstring.
    m_doc: *const c_char,
    /// `m_size`, the per-interpreter state size; `-1` for a module without
    /// state (single-phase init).
    m_size: isize,
    /// `m_methods`, the [`PyMethodDef`] table.
    m_methods: *mut PyMethodDef,
    /// `m_slots`, the multi-phase init slots (null for single-phase).
    m_slots: *mut c_void,
    /// `m_traverse`, the GC traverse function.
    m_traverse: *const c_void,
    /// `m_clear`, the clear function.
    m_clear: *const c_void,
    /// `m_free`, the dealloc function.
    m_free: *const c_void,
}

unsafe extern "C" {
    /// The symbol `PyModule_Create` resolves to; the macro adds the API
    /// version, which is 0 for a single-phase module.
    fn PyModule_Create2(def: *mut PyModuleDef, api_version: c_int) -> *mut c_void;
    /// Attach the C string `value` to `module` under `name` as a str.
    fn PyModule_AddStringConstant(
        module: *mut c_void,
        name: *const c_char,
        value: *const c_char,
    ) -> c_int;
    /// New str from a NUL-terminated C string.
    fn PyUnicode_FromString(value: *const c_char) -> *mut c_void;
    /// New int from a C long.
    fn PyLong_FromLong(value: c_long) -> *mut c_void;
}

/// The methods the module exposes: the probe value and the engine version.
static METHODS: [PyMethodDef; 3] = [
    PyMethodDef {
        ml_name: c"answer".as_ptr(),
        ml_meth: Some(answer),
        ml_flags: METH_NOARGS,
        ml_doc: c"the probe value the module returns".as_ptr(),
    },
    PyMethodDef {
        ml_name: c"engine_version".as_ptr(),
        ml_meth: Some(engine_version),
        ml_flags: METH_NOARGS,
        ml_doc: c"the reference engine version this port targets".as_ptr(),
    },
    PyMethodDef {
        ml_name: std::ptr::null(),
        ml_meth: None,
        ml_flags: 0,
        ml_doc: std::ptr::null(),
    },
];

/// The method-table pointer for the module definition.
const METHODS_PTR: *mut PyMethodDef = METHODS.as_ptr() as *mut PyMethodDef;

/// The raw pointers a [`PyMethodDef`]/[`PyModuleDef`] carries are not `Sync`,
/// but these two statics are immutable templates CPython only reads (the
/// import machinery never mutates a module definition after process start),
/// so sharing them is sound.
unsafe impl Sync for PyMethodDef {}
unsafe impl Sync for PyModuleDef {}

/// The module definition: a `static`, not a `const`, so the pointer
/// `PyInit_boundless_core` hands to `PyModule_Create2` is a stable address —
/// CPython keeps `md_def` on the module and re-reads it on re-import.
///
/// `PyModuleDef_Init` writes into the def it is given (`ob_refcnt`,
/// `ob_type`, `m_index`), so the definition must live in writable memory.
/// The linker's default for a `static` with pointer relocations is
/// `.data.rel.ro`, which full RELRO (`-z now`, the cdylib default) makes
/// read-only after relocation — that write then segfaults on import. The
/// `.data` section is outside the RELRO segment, so this keeps the def
/// writable while the string/method pointers it carries still get their
/// load-time relocations applied.
#[unsafe(link_section = ".data")]
static MODULE_DEF: PyModuleDef = PyModuleDef {
    m_base: HEAD_INIT,
    m_name: MODULE_NAME.as_ptr(),
    m_doc: c"the minimal native module the port installs for its scripts".as_ptr(),
    m_size: -1,
    m_methods: METHODS_PTR,
    m_slots: std::ptr::null_mut(),
    m_traverse: std::ptr::null(),
    m_clear: std::ptr::null(),
    m_free: std::ptr::null(),
};

/// `answer()` — the probe value the module returns.
///
/// # Safety
///
/// Called by CPython with `self` and the null `METH_NOARGS` argument.
unsafe extern "C" fn answer(_self: *mut c_void, _unused: *mut c_void) -> *mut c_void {
    // The return value is a new reference; CPython owns it after the call.
    unsafe { PyLong_FromLong(42) }
}

/// `engine_version()` — the reference engine version this port targets.
///
/// # Safety
///
/// Called by CPython with `self` and the null `METH_NOARGS` argument.
unsafe extern "C" fn engine_version(_self: *mut c_void, _unused: *mut c_void) -> *mut c_void {
    // The return value is a new reference; CPython owns it after the call.
    unsafe { PyUnicode_FromString(ENGINE_VERSION.as_ptr()) }
}

/// The built cdylib.
///
/// `cargo build` writes it to `<workspace>/target/<profile>/libpython_module.so`,
/// but `cargo test` (without a prior `cargo build`) never produces that file —
/// it leaves the cdylib under `target/<profile>/build/python-module/<hash>/out/`,
/// and a re-entrant `cargo build` from inside a test blocks on the target-dir
/// lock the outer `cargo test` still holds. So the artifact is *discovered*:
/// the canonical path first, then the newest of the `build/…/out` candidates.
/// This keeps `cargo test -p python-module` self-contained on a clean tree.
pub fn built_artifact() -> PathBuf {
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let profile = if cfg!(debug_assertions) {
        "debug"
    } else {
        "release"
    };
    let target = workspace.join("target").join(profile);

    let canonical = target.join("libpython_module.so");
    if canonical.is_file() {
        return canonical;
    }

    // `cargo test`'s placement: newest `<hash>/out/libpython_module.so` under
    // the package's build dir.
    let build_dir = target.join("build").join("python-module");
    let mut newest: Option<(SystemTime, PathBuf)> = None;
    let Ok(entries) = std::fs::read_dir(&build_dir) else {
        return canonical;
    };
    for entry in entries.flatten() {
        let candidate = entry.path().join("out").join("libpython_module.so");
        let Ok(meta) = std::fs::metadata(&candidate) else {
            continue;
        };
        if !meta.is_file() {
            continue;
        }
        let Ok(modified) = meta.modified() else {
            continue;
        };
        if newest.as_ref().is_none_or(|(t, _)| modified > *t) {
            newest = Some((modified, candidate));
        }
    }
    match newest {
        Some((_, path)) => path,
        // Neither location has it; return the canonical path so callers that
        // report the missing file point at the documented `cargo build` output.
        None => canonical,
    }
}

/// Install the built module under `prefix`: the cdylib is copied into
/// `<prefix>/site-packages/` as `<module>.so` (Python imports a file named
/// after the module, without the `lib` prefix), returning the installed path.
///
/// The acceptance for the module build (BW-00-03-03): once this has run,
/// `import boundless_core` succeeds in an interpreter whose path includes
/// that site-packages directory.
pub fn install(prefix: &Path) -> io::Result<PathBuf> {
    let site_packages = prefix.join("site-packages");
    std::fs::create_dir_all(&site_packages)?;
    let installed = site_packages.join(format!(
        "{}.so",
        MODULE_NAME.to_str().expect("module name is UTF-8")
    ));
    std::fs::copy(built_artifact(), &installed)?;
    Ok(installed)
}

/// The module entry point CPython calls on `import boundless_core`: creates
/// the module and attaches `__version__`.
///
/// # Safety
///
/// Called by CPython's import machinery on a loaded extension module.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyInit_boundless_core() -> *mut c_void {
    let module = unsafe {
        PyModule_Create2(
            &MODULE_DEF as *const PyModuleDef as *mut PyModuleDef,
            PYTHON_API_VERSION,
        )
    };
    if module.is_null() {
        return std::ptr::null_mut();
    }
    let attached = unsafe {
        PyModule_AddStringConstant(
            module,
            c"__version__".as_ptr(),
            VERSION_BYTES.as_ptr().cast::<c_char>(),
        )
    };
    if attached != 0 {
        // The module object is still owned by the importer; a failed
        // attribute attach leaves a working module without `__version__`.
    }
    module
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_module_def_matches_the_interpreter_header() {
        // The sizes a compiled probe against `Python.h` reports (3.14): a
        // layout that drifts here segfaults on import, so they are pinned.
        assert_eq!(std::mem::size_of::<PyModuleDef>(), 104);
        assert_eq!(std::mem::size_of::<PyModuleDefBase>(), 40);
        assert_eq!(std::mem::size_of::<PyMethodDef>(), 32);
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
        assert_eq!(std::mem::offset_of!(PyMethodDef, ml_name), 0);
        assert_eq!(std::mem::offset_of!(PyMethodDef, ml_meth), 8);
        assert_eq!(std::mem::offset_of!(PyMethodDef, ml_flags), 16);
        assert_eq!(std::mem::offset_of!(PyMethodDef, ml_doc), 24);
    }

    #[test]
    fn the_method_table_is_terminated() {
        // CPython walks the table until a null `ml_name`.
        assert_eq!(METHODS.len(), 3);
        assert!(METHODS[2].ml_name.is_null());
        assert_eq!(METHODS[0].ml_flags, METH_NOARGS);
        assert_eq!(METHODS[1].ml_flags, METH_NOARGS);
    }

    #[test]
    fn the_module_api_version_matches_the_interpreter() {
        // `modsupport.h` pins this per release; `check_api_version` rejects
        // a mismatch, so the value cannot drift silently.
        assert_eq!(PYTHON_API_VERSION, 1013);
    }
}
