//! BW-00-03-03: the minimal importable Python module the build entry
//! produces.
//!
//! This is the "Python module" half of `BW-00-03` (the build entry that is
//! "Cargo workspace, Python module, startup entry"). It is deliberately a
//! *separate* artifact from the embedded host: `python-host` runs the
//! interpreter *inside* a role process and registers the engine modules
//! (`BigWorld`, `Math`, …) into it through init-time jobs; this crate is the
//! module the build *ships*, the thing a deployed install can put on the
//! Python path and `import`. So the acceptance "after install the minimal
//! module imports" is proven against a real interpreter, not against the
//! embedded one.
//!
//! Scope: one importable module named `boundless`, exposing the module
//! version as `__version__` and a single `version()` call, so the import is
//! verifiable beyond merely resolving the symbol. The surface is kept to the
//! stable ABI: the same `libboundless.so` loads into whatever CPython 3.x the
//! target has, which is exactly what a build entry should guarantee before it
//! grows into the real engine modules of lane BW-03.

mod capi;

use capi::*;
use std::ffi::c_char;

/// The module's version as a NUL-terminated buffer for the C API, which
/// takes no lengths: Rust strings are length-delimited, so the terminator is
/// appended at compile time rather than by hand at each call site.
const VERSION_NUL: &[u8] = concat!(env!("CARGO_PKG_VERSION"), "\0").as_bytes();

/// `boundless.version()`: returns the module version.
unsafe extern "C" fn version(_self_: *mut PyObject, _args: *mut PyObject) -> *mut PyObject {
    unsafe { PyUnicode_FromString(VERSION_NUL.as_ptr() as *const c_char) }
}

/// The method table: the `version` row plus the zeroed terminator.
static METHODS: [PyMethodDef; 2] = [
    PyMethodDef {
        ml_name: c"version".as_ptr() as *const c_char,
        ml_meth: Some(version),
        ml_flags: METH_VARARGS,
        ml_doc: c"Return the module version.".as_ptr() as *const c_char,
    },
    // The zeroed terminator row `PyModuleDef` method tables require.
    PyMethodDef {
        ml_name: std::ptr::null(),
        ml_meth: None,
        ml_flags: 0,
        ml_doc: std::ptr::null(),
    },
];

/// The module definition, `m_size = -1` (single-phase init), which is what a
/// plain extension module ships.
///
/// This has to be `static mut`, not `static`: CPython writes into the
/// definition at create time — `PyModuleDef_Init` stores `ob_refcnt`,
/// `ob_type` and a fresh `m_index` into it — and keeps a pointer to it as
/// `module.md_def` for the module's whole lifetime (`_PyModule_IsExtension`
/// reads `md_def->m_methods`). A Rust `static` lands in a read-only section
/// (`RELRRO`), so the first write faults; and a stack/heap copy would dangle
/// the moment `PyInit_boundless` returns. The `m_methods` table it points to
/// stays a read-only `static`, since the C API only reads that one.
static mut MODULE_DEF: PyModuleDef = PyModuleDef {
    m_base: PyModuleDefBase {
        ob_refcnt: 1,
        ob_type: std::ptr::null_mut(),
        m_init: None,
        m_index: 0,
        m_copy: std::ptr::null_mut(),
    },
    m_name: c"boundless".as_ptr() as *const c_char,
    m_doc: c"A minimal module built by the Boundless build entry.".as_ptr() as *const c_char,
    m_size: -1,
    m_methods: METHODS.as_ptr() as *mut PyMethodDef,
    m_slots: std::ptr::null_mut(),
    m_traverse: None,
    m_clear: None,
    m_free: None,
};

/// `PyInit_boundless`: the import entry point CPython calls when the module
/// is loaded. Note that `PyModule_Create2` does *not* copy the definition: it
/// initialises it in place and keeps a pointer to it as `module.md_def`,
/// which is what forces `MODULE_DEF` to be writable and outlive this call.
#[unsafe(no_mangle)]
pub extern "C" fn PyInit_boundless() -> *mut PyObject {
    // SAFETY: the mutable definition is the module's own, single-threaded
    // (only ever touched from the interpreter that owns it), and `md_def`
    // keeps it alive.
    let module = unsafe { PyModule_Create2(&raw mut MODULE_DEF, PYTHON_API_VERSION) };
    if module.is_null() {
        return std::ptr::null_mut();
    }
    // The string is copied by the C API; a failure only leaves `__version__`
    // absent, which the import test asserts against.
    let _ = unsafe {
        PyModule_AddStringConstant(
            module,
            c"__version__".as_ptr() as *const c_char,
            VERSION_NUL.as_ptr() as *const c_char,
        )
    };
    module
}

/// The ABI entry point is `extern "C"` and never returns a value, so there is
/// no Rust-only body to exercise in unit tests; the behaviour is proven by the
/// integration test (`tests/import.rs`) that loads the built `cdylib` into a
/// real interpreter. The module is still `rlib`-buildable so the package's
/// tests can reference the constants (e.g. `VERSION_NUL`) without a live
/// Python.
#[cfg(test)]
mod tests {
    use super::VERSION_NUL;

    #[test]
    fn the_version_is_a_nonempty_terminated_string() {
        assert!(!VERSION_NUL.is_empty());
        assert!(VERSION_NUL.ends_with(&[0]));
    }
}
