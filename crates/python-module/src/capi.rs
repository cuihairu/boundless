//! The stable-ABI CPython C surface `PyInit_boundless` is built from.
//!
//! The extension module deliberately touches only the *limited* (stable) C
//! API, so the same `libboundless.so` loads into the interpreter the build
//! machine links and into any other CPython 3.x on the same platform. The
//! struct layouts below are the ABI-stable `PyModuleDef`/`PyMethodDef`/
//! `PyObject` shapes, not the versioned internal ones the embedded
//! `python-host` links against — those are declared in that crate instead.
//!
//! Reference shapes, as compiled by CPython 3.14 (stable across 3.x):
//! `Python.h` → `moduleobject.h`, `methodobject.h`, `object.h`.

use std::ffi::{c_char, c_int};
use std::os::raw::c_void;

/// `Py_ssize_t`, the C API's signed index type.
pub type PySsize = isize;

/// `PyObject`: opaque here. The module only carries handles across the
/// boundary and never reads the header, so the layout is intentionally not
/// spelled out (the embedded host spells it out in its own `capi`).
#[repr(C)]
pub struct PyObject {
    _private: (),
}

/// `PyTypeObject`: opaque, same reason as `PyObject`.
#[repr(C)]
pub struct PyTypeObject {
    _private: (),
}

/// `PyCFunction` (`methodobject.h:19`), the `METH_VARARGS` callback shape.
pub type PyCFunction = unsafe extern "C" fn(*mut PyObject, *mut PyObject) -> *mut PyObject;

/// `PyMethodDef` (`methodobject.h:68`): one row of a module's method table.
#[repr(C)]
pub struct PyMethodDef {
    /// `ml_name`: the Python-visible name, NUL-terminated.
    pub ml_name: *const c_char,
    /// `ml_meth`: the C callback.
    pub ml_meth: Option<PyCFunction>,
    /// `ml_flags`: `METH_*` bit mask.
    pub ml_flags: c_int,
    /// `ml_doc`: the docstring, NUL-terminated (may be null).
    pub ml_doc: *const c_char,
}

/// `PyModuleDef_Base` (`moduleobject.h:39`): the `PyObject_HEAD` prefix plus
/// the three fields `PyModuleDef_HEAD_INIT` zeroes.
#[repr(C)]
pub struct PyModuleDefBase {
    /// `ob_refcnt` from `PyObject_HEAD_INIT(1)`.
    pub ob_refcnt: PySsize,
    /// `ob_type` from `PyObject_HEAD_INIT(NULL)`.
    pub ob_type: *mut PyTypeObject,
    /// `m_init`: null for a single static definition.
    pub m_init: Option<unsafe extern "C" fn() -> *mut PyObject>,
    /// `m_index`: zero for a fresh definition.
    pub m_index: PySsize,
    /// `m_copy`: null for a fresh definition.
    pub m_copy: *mut PyObject,
}

/// `PyModuleDef_Slot` (`moduleobject.h:69`): a slot entry, unused here.
#[repr(C)]
pub struct PyModuleDefSlot {
    pub slot: c_int,
    pub value: *mut c_void,
}

/// `PyModuleDef` (`moduleobject.h:107`): the module's static description.
#[repr(C)]
pub struct PyModuleDef {
    /// `m_base`: the object header + re-init bookkeeping.
    pub m_base: PyModuleDefBase,
    /// `m_name`: the module name, NUL-terminated.
    pub m_name: *const c_char,
    /// `m_doc`: the module docstring, NUL-terminated.
    pub m_doc: *const c_char,
    /// `m_size`: -1 marks a legacy (single-phase-init) module.
    pub m_size: PySsize,
    /// `m_methods`: the method table, terminated by a zeroed row.
    pub m_methods: *mut PyMethodDef,
    /// `m_slots`: null (no slot table).
    pub m_slots: *mut PyModuleDefSlot,
    /// `m_traverse`: null.
    pub m_traverse: Option<unsafe extern "C" fn() -> c_int>,
    /// `m_clear`: null.
    pub m_clear: Option<unsafe extern "C" fn() -> c_int>,
    /// `m_free`: null.
    pub m_free: Option<unsafe extern "C" fn() -> ()>,
}

// SAFETY: these definitions are `static` descriptions the module only ever
// reads. CPython itself treats them as immutable: `PyModule_Create2` copies
// the definition and records nothing back into it (the `m_init`/`m_index`
// bookkeeping is written into the copy), and nothing in this crate mutates
// the tables. The `Sync` impls below are what let them sit in `static`s.
unsafe impl Sync for PyMethodDef {}
unsafe impl Sync for PyModuleDefBase {}
unsafe impl Sync for PyModuleDef {}

/// `METH_VARARGS` (`methodobject.h:95`): the callback takes `(self, args)`.
pub const METH_VARARGS: c_int = 0x0001;

/// `PYTHON_API_VERSION` (`modsupport.h:59`): the ABI version handed to
/// `PyModule_Create2`.
pub const PYTHON_API_VERSION: c_int = 1013;

unsafe extern "C" {
    /// `PyModule_Create2` (`modsupport.h:111`): allocate the module object
    /// from a static `PyModuleDef`.
    pub fn PyModule_Create2(def: *mut PyModuleDef, apiver: c_int) -> *mut PyObject;

    /// `PyModule_AddStringConstant` (`modsupport.h:40`): bind a string
    /// attribute (copies the bytes; no reference to manage).
    pub fn PyModule_AddStringConstant(
        module: *mut PyObject,
        name: *const c_char,
        value: *const c_char,
    ) -> c_int;

    /// `PyUnicode_FromString`: build a str object from a NUL-terminated
    /// buffer.
    pub fn PyUnicode_FromString(s: *const c_char) -> *mut PyObject;
}
