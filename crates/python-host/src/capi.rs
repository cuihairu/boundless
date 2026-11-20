//! The CPython C API surface `python-host` links against, declared by hand.
//!
//! Only the symbols the ported subsystems touch are declared, so a CPython
//! revision bump is a one-file review. Reference side: `lib/pyscript/` uses
//! the same API through `pch.hpp` under the 3.13 tree; the 3.13 migration
//! ledger (`BigWorld/docs/python-313-migration.md`) records which 2.7 symbols
//! vanished, and this module only names what still exists there:
//! `PySys_SetArgv` and `_PyModule_Clear` are gone, `PyEval_InitThreads` is
//! gone, and `Py_TabcheckFlag` is gone.
//!
//! The interpreter is configured the way the reference does it -- the legacy
//! pre-init globals (`Py_FrozenFlag`, `Py_NoSiteFlag`,
//! `Py_IgnoreEnvironmentFlag`) -- because the reference deliberately did not
//! migrate `Script::init` to `PyConfig`; the ledger tracks that as an open
//! behaviour-level change. `_PyConfig_InitCompatConfig` still maps those
//! globals onto a `PyConfig`, so the flags keep working on 3.13 and later.

#![allow(
    non_snake_case,
    non_camel_case_types,
    missing_docs,
    clippy::missing_safety_doc
)]

use std::os::raw::{c_char, c_int};
use std::sync::atomic::{AtomicBool, Ordering};

/// Set once `Py_Finalize` has run. `PyRef::drop` consults it so a handle that
/// outlives the interpreter never decrements into a torn-down allocator.
static FINALIZED: AtomicBool = AtomicBool::new(false);

/// `Script::isFinalised` for the handle layer.
pub fn is_finalized() -> bool {
    FINALIZED.load(Ordering::SeqCst)
}

/// Mark the interpreter as gone, right after `Py_Finalize` returns.
pub fn mark_finalized() {
    FINALIZED.store(true, Ordering::SeqCst);
}

/// `PyObject`. The two leading fields are stable across the 3.x line and are
/// what `Py_TYPE` and the reference's `ob_refcnt == 0` assertion need.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct PyObject {
    ob_refcnt: isize,
    ob_type: *mut PyTypeObject,
}

/// `PyTypeObject`. Never dereferenced here: it is compared through
/// `Py_TYPE` and handed back to CPython.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct PyTypeObject {
    _opaque: [u8; 0],
}

/// `Py_ssize_t`.
pub type PySsize = isize;

/// `Py_None`, `_Py_NoneStruct` in the C API.
pub fn py_none() -> *mut PyObject {
    std::ptr::addr_of_mut!(_Py_NoneStruct)
}

/// `Py_True`.
pub fn py_true() -> *mut PyObject {
    std::ptr::addr_of_mut!(_Py_TrueStruct)
}

/// `Py_False`.
pub fn py_false() -> *mut PyObject {
    std::ptr::addr_of_mut!(_Py_FalseStruct)
}

/// The three legacy pre-init globals, read back where init writes them.
///
/// `Py_FrozenFlag` has no `sys.flags` field (it is Android-only in the C
/// API), so a caller observing the write reads it here.
pub fn pre_init_flags() -> (c_int, c_int, c_int) {
    unsafe {
        (
            std::ptr::addr_of!(Py_FrozenFlag).read(),
            std::ptr::addr_of!(Py_NoSiteFlag).read(),
            std::ptr::addr_of!(Py_IgnoreEnvironmentFlag).read(),
        )
    }
}

/// `PyModule_Check`, a macro in the C API (`PyObject_TypeCheck` against
/// `&PyModule_Type`).
///
/// SAFETY: `obj` must be a borrowed object CPython gave out, or null.
pub unsafe fn py_module_check(obj: *mut PyObject) -> bool {
    if obj.is_null() {
        return false;
    }
    unsafe { PyType_IsSubtype(Py_TYPE(obj), std::ptr::addr_of_mut!(PyModule_Type)) == 1 }
}

/// `PyErr_Clear`, called without touching the exception state.
pub unsafe fn clear_error() {
    unsafe { PyErr_Clear() }
}

/// `Py_IncRef`. Null pointers are a no-op, matching the C `Py_XINCREF`.
pub unsafe fn inc_ref(obj: *mut PyObject) {
    unsafe {
        if !obj.is_null() {
            Py_IncRef(obj);
        }
    }
}

/// `Py_DecRef` for a possibly-null object. Null is skipped so `PyRef::drop`
/// can use one path for both the empty handle and a borrowed one.
pub unsafe fn dec_ref(obj: *mut PyObject) {
    unsafe {
        if !obj.is_null() {
            Py_DecRef(obj);
        }
    }
}

/// `str(obj)`: the `PyObject_Str` result read back as a Rust string.
pub unsafe fn to_string(obj: *mut PyObject) -> String {
    unsafe {
        if obj.is_null() {
            return String::new();
        }
        let mut size: PySsize = 0;
        let pointer = PyUnicode_AsUTF8AndSize(obj, &mut size);
        if pointer.is_null() {
            return String::new();
        }
        std::str::from_utf8(std::slice::from_raw_parts(
            pointer as *const u8,
            size as usize,
        ))
        .unwrap_or_default()
        .to_owned()
    }
}

unsafe extern "C" {
    pub static mut _Py_NoneStruct: PyObject;
    pub static mut _Py_TrueStruct: PyObject;
    pub static mut _Py_FalseStruct: PyObject;
    pub static mut Py_FrozenFlag: c_int;
    pub static mut Py_NoSiteFlag: c_int;
    pub static mut Py_IgnoreEnvironmentFlag: c_int;
    pub static mut PyModule_Type: PyTypeObject;
}

unsafe extern "C" {
    pub fn Py_IsInitialized() -> c_int;
    pub fn Py_Initialize();
    pub fn Py_Finalize();
    pub fn Py_DecRef(obj: *mut PyObject);
    pub fn Py_IncRef(obj: *mut PyObject);
    pub fn PyGILState_Ensure() -> c_int;
    pub fn PyGILState_Release(state: c_int);
    pub fn PyRun_SimpleString(string: *const c_char) -> c_int;
    pub fn PySys_SetObject(name: *const c_char, value: *mut PyObject) -> c_int;
    pub fn PySys_GetObject(name: *const c_char) -> *mut PyObject;
    pub fn PyList_New(size: PySsize) -> *mut PyObject;
    pub fn PyList_SetItem(list: *mut PyObject, index: PySsize, item: *mut PyObject) -> c_int;
    pub fn PyUnicode_FromString(string: *const c_char) -> *mut PyObject;
    pub fn PyDict_Copy(obj: *mut PyObject) -> *mut PyObject;
    pub fn PyDict_SetItem(dict: *mut PyObject, key: *mut PyObject, value: *mut PyObject) -> c_int;
    pub fn PyGC_Collect() -> PySsize;
    pub fn PyDict_GetItemString(dict: *mut PyObject, key: *const c_char) -> *mut PyObject;
    pub fn PyDict_SetItemString(
        dict: *mut PyObject,
        key: *const c_char,
        value: *mut PyObject,
    ) -> c_int;
    pub fn PyDict_Next(
        dict: *mut PyObject,
        position: *mut PySsize,
        key: *mut *mut PyObject,
        value: *mut *mut PyObject,
    ) -> c_int;
    pub fn PyUnicode_CompareWithASCIIString(unicode: *mut PyObject, ascii: *const c_char) -> c_int;
    pub fn PyUnicode_AsUTF8AndSize(unicode: *mut PyObject, size: *mut PySsize) -> *mut c_char;
    pub fn Py_TYPE(obj: *mut PyObject) -> *mut PyTypeObject;
    pub fn PyType_IsSubtype(derived: *mut PyTypeObject, base: *mut PyTypeObject) -> c_int;
    pub fn PyModule_GetDict(module: *mut PyObject) -> *mut PyObject;
    pub fn PyImport_GetModuleDict() -> *mut PyObject;
    pub fn PyErr_Clear();
    pub fn PyErr_Occurred() -> *mut PyObject;
    pub fn PyErr_PrintEx(skip: c_int);
    pub fn PyErr_Fetch(
        type_: *mut *mut PyObject,
        value: *mut *mut PyObject,
        traceback: *mut *mut PyObject,
    );
    pub fn PyErr_Restore(type_: *mut PyObject, value: *mut PyObject, traceback: *mut PyObject);
    pub fn PyErr_Format(exception: *mut PyTypeObject, message: *const c_char) -> c_int;
    pub fn PyThreadState_Get() -> *mut std::os::raw::c_void;
    pub fn PyImport_AddModule(name: *const c_char) -> *mut PyObject;
    pub fn PyImport_ImportModule(name: *const c_char) -> *mut PyObject;
    pub fn PyObject_Str(obj: *mut PyObject) -> *mut PyObject;
    pub fn PyObject_GetAttrString(obj: *mut PyObject, attr_name: *const c_char) -> *mut PyObject;
    pub fn PyObject_CallNoArgs(func: *mut PyObject) -> *mut PyObject;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `PyModule_Check` on a null result is `false`, never a dereference: a
    /// failed `PyDict_GetItemString` is exactly the case the check exists
    /// for.
    #[test]
    fn module_check_rejects_null() {
        assert!(!unsafe { py_module_check(std::ptr::null_mut()) });
    }

    /// `Py_XINCREF`/`Py_XDECREF` on null are no-ops, so the handle layer can
    /// run both through one code path.
    #[test]
    fn null_reference_ops_are_noops() {
        unsafe {
            inc_ref(std::ptr::null_mut());
            dec_ref(std::ptr::null_mut());
        }
    }

    /// The three singletons are real addresses, so the host can compare
    /// against them with a plain pointer equality.
    #[test]
    fn the_singletons_are_non_null() {
        assert!(!py_none().is_null());
        assert!(!py_true().is_null());
        assert!(!py_false().is_null());
    }

    /// A null result renders as the empty string, so a failed `PyObject_Str`
    /// degrades to silence instead of a panic.
    #[test]
    fn a_null_object_renders_empty() {
        assert_eq!(unsafe { to_string(std::ptr::null_mut()) }, String::new());
    }
}
