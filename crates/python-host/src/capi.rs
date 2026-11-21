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
    non_upper_case_globals,
    missing_docs,
    clippy::missing_safety_doc
)]

pub use std::os::raw::{c_char, c_int, c_uint};
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

/// The head of a `PyTypeObject` up to `tp_name`: the `PyObject_VAR_HEAD`
/// plus the name pointer, the one field the port reads straight out of the
/// type -- the reference's read-only attribute sentence formats `typeName()`,
/// which is `tp_name` (`pyobject_plus.hpp:443-454`). The layout is fixed by
/// the CPython ABI.
#[repr(C)]
pub struct PyTypeHead {
    ob_refcnt: isize,
    ob_type: *mut PyTypeObject,
    ob_size: PySsize,
    tp_name: *const c_char,
}

/// `typeName()`: the type's `tp_name`, `Math.Vector3` style with the module
/// prefix.
///
/// # Safety
///
/// `type_` must be a live type object.
pub unsafe fn type_name(type_: *mut PyTypeObject) -> String {
    // SAFETY: the head layout above is the CPython ABI and the name pointer
    // is a live NUL-terminated static string.
    let name = unsafe { (*(type_.cast::<PyTypeHead>())).tp_name };
    unsafe { std::ffi::CStr::from_ptr(name) }
        .to_string_lossy()
        .into_owned()
}

/// `Py_ssize_t`.
pub type PySsize = isize;

/// `Py_None`, `_Py_NoneStruct` in the C API.
pub fn py_none() -> *mut PyObject {
    std::ptr::addr_of_mut!(_Py_NoneStruct)
}

/// `Py_NotImplemented`, `_Py_NotImplementedStruct` in the C API.
pub fn py_not_implemented() -> *mut PyObject {
    std::ptr::addr_of_mut!(_Py_NotImplementedStruct)
}

/// `PyObject_TypeCheck` against `PyFloat_Type`: a float or a subtype.
///
/// SAFETY: `obj` must be a borrowed object CPython gave out, or null.
pub unsafe fn py_float_check(obj: *mut PyObject) -> bool {
    if obj.is_null() {
        return false;
    }
    unsafe { PyType_IsSubtype(Py_TYPE(obj), std::ptr::addr_of_mut!(PyFloat_Type)) == 1 }
}

/// `PyObject_TypeCheck` against `PyLong_Type`: an int or a subtype.
///
/// SAFETY: `obj` must be a borrowed object CPython gave out, or null.
pub unsafe fn py_long_check(obj: *mut PyObject) -> bool {
    if obj.is_null() {
        return false;
    }
    unsafe { PyType_IsSubtype(Py_TYPE(obj), std::ptr::addr_of_mut!(PyLong_Type)) == 1 }
}

/// `PyObject_TypeCheck` against `PyTuple_Type`.
///
/// SAFETY: `obj` must be a borrowed object CPython gave out, or null.
pub unsafe fn py_tuple_check(obj: *mut PyObject) -> bool {
    if obj.is_null() {
        return false;
    }
    unsafe { PyType_IsSubtype(Py_TYPE(obj), std::ptr::addr_of_mut!(PyTuple_Type)) == 1 }
}

/// `PyObject_TypeCheck` against `PyList_Type`.
///
/// SAFETY: `obj` must be a borrowed object CPython gave out, or null.
pub unsafe fn py_list_check(obj: *mut PyObject) -> bool {
    if obj.is_null() {
        return false;
    }
    unsafe { PyType_IsSubtype(Py_TYPE(obj), std::ptr::addr_of_mut!(PyList_Type)) == 1 }
}

/// `PyObject_TypeCheck` against `PyBytes_Type`.
///
/// SAFETY: `obj` must be a borrowed object CPython gave out, or null.
pub unsafe fn py_bytes_check(obj: *mut PyObject) -> bool {
    if obj.is_null() {
        return false;
    }
    unsafe { PyType_IsSubtype(Py_TYPE(obj), std::ptr::addr_of_mut!(PyBytes_Type)) == 1 }
}

/// `PyObject_TypeCheck` against `PyUnicode_Type`: a str or a subtype.
///
/// SAFETY: `obj` must be a borrowed object CPython gave out, or null.
pub unsafe fn py_str_check(obj: *mut PyObject) -> bool {
    if obj.is_null() {
        return false;
    }
    unsafe { PyType_IsSubtype(Py_TYPE(obj), std::ptr::addr_of_mut!(PyUnicode_Type)) == 1 }
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

/// `PyTraceBack_Check`, a macro in the C API (`PyObject_TypeCheck` against
/// `&PyTraceBack_Type`).
///
/// SAFETY: `obj` must be a borrowed object CPython gave out, or null.
pub unsafe fn py_traceback_check(obj: *mut PyObject) -> bool {
    if obj.is_null() {
        return false;
    }
    unsafe { PyType_IsSubtype(Py_TYPE(obj), std::ptr::addr_of_mut!(PyTraceBack_Type)) == 1 }
}

/// `PyObjectPlus::refCount()` (`pyobject_plus.hpp:1100`) and the cstdmf
/// `hasZeroReferenceCount` specialisation that reads `ob_refcnt` directly
/// (`pyobject_pointer.hpp:40`): the reference inspects the count for its own
/// assertions, and the port's tests do the same.
///
/// SAFETY: `obj` must be a borrowed object CPython gave out, or null.
pub unsafe fn refcount(obj: *mut PyObject) -> isize {
    if obj.is_null() {
        return 0;
    }
    unsafe { std::ptr::addr_of!((*obj).ob_refcnt).read() }
}

/// `PyCFunction`: a builtin function object's implementation (`ml_meth`).
pub type PyCFunction = unsafe extern "C" fn(*mut PyObject, *mut PyObject) -> *mut PyObject;

/// `PyCapsule_Destructor`, the `destructor` argument of `PyCapsule_New`.
pub type PyCapsuleDestructor = unsafe extern "C" fn(*mut PyObject);

/// A `PyGetSetDef` getter (`descrobject.h`).
pub type Getter = unsafe extern "C" fn(*mut PyObject, *mut std::os::raw::c_void) -> *mut PyObject;

/// A `PyGetSetDef` setter (`descrobject.h`); a negative return means the
/// assignment failed with a pending exception.
pub type Setter =
    unsafe extern "C" fn(*mut PyObject, *mut PyObject, *mut std::os::raw::c_void) -> c_int;

/// `tp_new`: the construction entry a type may refuse.
pub type NewFunction =
    unsafe extern "C" fn(*mut PyTypeObject, *mut PyObject, *mut PyObject) -> *mut PyObject;

/// `reprfunc`.
pub type ReprFunction = unsafe extern "C" fn(*mut PyObject) -> *mut PyObject;

/// `destructor`, the `tp_dealloc` handler.
pub type Destructor = unsafe extern "C" fn(*mut PyObject);

/// `richcmpfunc`, the `tp_richcompare` handler; `op` is one of the `Py_*`
/// comparison opcodes.
pub type RichCompareFunction =
    unsafe extern "C" fn(*mut PyObject, *mut PyObject, c_int) -> *mut PyObject;

/// `binaryfunc`, the two-operand number handler (`nb_add` and friends).
pub type BinaryFunction = unsafe extern "C" fn(*mut PyObject, *mut PyObject) -> *mut PyObject;

/// `unaryfunc`, the one-operand number handler (`nb_negative` and friends).
pub type UnaryFunction = unsafe extern "C" fn(*mut PyObject) -> *mut PyObject;

/// `inquiry`, the truth-value handler (`nb_bool`).
pub type InquiryFunction = unsafe extern "C" fn(*mut PyObject) -> c_int;

/// `ternaryfunc`, the `tp_call` handler: callee, argument tuple, keywords.
pub type TernaryFunction =
    unsafe extern "C" fn(*mut PyObject, *mut PyObject, *mut PyObject) -> *mut PyObject;

/// `lenfunc`, the `sq_length`/`mp_length` handler.
pub type LenFunction = unsafe extern "C" fn(*mut PyObject) -> PySsize;

/// `ssizeargfunc`, the `sq_item`/`sq_repeat` handler.
pub type SsizeArgFunction = unsafe extern "C" fn(*mut PyObject, PySsize) -> *mut PyObject;

/// `ssizeobjargproc`, the `sq_ass_item` handler; a `NULL` value is the
/// sequence protocol's spelling of deletion.
pub type SsizeObjArgFunction = unsafe extern "C" fn(*mut PyObject, PySsize, *mut PyObject) -> c_int;

/// `METH_VARARGS` from `Include/methodobject.h`.
pub const METH_VARARGS: c_int = 0x0001;

/// `PyGetSetDef`, one attribute descriptor of a type.
#[repr(C)]
pub struct PyGetSetDef {
    /// `name`.
    pub name: *const c_char,
    /// `get`.
    pub get: Option<Getter>,
    /// `set`, absent for a read-only attribute.
    pub set: Option<Setter>,
    /// `doc`.
    pub doc: *const c_char,
    /// `closure`, handed to every call of this attribute.
    pub closure: *mut std::os::raw::c_void,
}

/// One `PyType_Slot` entry of a `PyType_Spec` (`object.h`).
#[repr(C)]
pub struct PyTypeSlot {
    /// The slot id, one of the `Py_tp_*` values.
    pub slot: c_int,
    /// The slot's function or data pointer.
    pub pfunc: *mut std::os::raw::c_void,
}

/// `PyType_Spec`, the `PyType_FromSpec` description of a heap type.
#[repr(C)]
pub struct PyTypeSpec {
    /// The dotted type name.
    pub name: *const c_char,
    /// Zero takes the base's size.
    pub basicsize: c_int,
    /// Zero for a fixed-size type.
    pub itemsize: c_int,
    /// `Py_TPFLAGS_*`; the default set is zero on this build.
    pub flags: c_uint,
    /// Terminated by `slot == 0`.
    pub slots: *mut PyTypeSlot,
}

/// `Py_tp_getattro` (`typeslots.h`).
pub const Py_tp_getattro: c_int = 58;
/// `Py_tp_setattro` (`typeslots.h`).
pub const Py_tp_setattro: c_int = 69;
/// `Py_nb_add` (`typeslots.h`).
pub const Py_nb_add: c_int = 7;
/// `Py_nb_subtract` (`typeslots.h`).
pub const Py_nb_subtract: c_int = 36;
/// `Py_nb_multiply` (`typeslots.h`).
pub const Py_nb_multiply: c_int = 29;
/// `Py_nb_negative` (`typeslots.h`).
pub const Py_nb_negative: c_int = 30;
/// `Py_nb_positive` (`typeslots.h`).
pub const Py_nb_positive: c_int = 32;
/// `Py_nb_bool` (`typeslots.h`).
pub const Py_nb_bool: c_int = 9;
/// `Py_nb_true_divide` (`typeslots.h`).
pub const Py_nb_true_divide: c_int = 37;
/// `Py_nb_inplace_add` (`typeslots.h`).
pub const Py_nb_inplace_add: c_int = 14;
/// `Py_nb_inplace_subtract` (`typeslots.h`).
pub const Py_nb_inplace_subtract: c_int = 23;
/// `Py_nb_inplace_multiply` (`typeslots.h`).
pub const Py_nb_inplace_multiply: c_int = 18;
/// `Py_nb_inplace_true_divide` (`typeslots.h`).
pub const Py_nb_inplace_true_divide: c_int = 24;
/// `Py_tp_richcompare` (`typeslots.h`).
pub const Py_tp_richcompare: c_int = 67;
/// `Py_tp_str` (`typeslots.h`).
pub const Py_tp_str: c_int = 70;
/// `Py_tp_call` (`typeslots.h`).
pub const Py_tp_call: c_int = 50;
/// `Py_tp_dealloc` (`typeslots.h`).
pub const Py_tp_dealloc: c_int = 52;

/// The rich-comparison operation codes of `Py_LT`..`Py_GE`
/// (`Include/object.h`).
pub const Py_LT: c_int = 0;
/// `Py_LE`.
pub const Py_LE: c_int = 1;
/// `Py_EQ`.
pub const Py_EQ: c_int = 2;
/// `Py_NE`.
pub const Py_NE: c_int = 3;
/// `Py_GT`.
pub const Py_GT: c_int = 4;
/// `Py_GE`.
pub const Py_GE: c_int = 5;
/// `Py_tp_doc` (`typeslots.h`).
pub const Py_tp_doc: c_int = 56;
/// `Py_tp_methods` (`typeslots.h`).
pub const Py_tp_methods: c_int = 64;
/// `Py_tp_new` (`typeslots.h`).
pub const Py_tp_new: c_int = 65;
/// `Py_tp_repr` (`typeslots.h`).
pub const Py_tp_repr: c_int = 66;
/// `PyMappingMethods` (`cpython/object.h`): the mapping protocol table.
#[repr(C)]
pub struct PyMappingMethods {
    /// `len(obj)`.
    pub mp_length: Option<LenFunction>,
    /// `obj[key]`.
    pub mp_subscript: Option<ReprFunction>,
    /// `obj[key] = value`, with a `NULL` value spelling deletion.
    pub mp_ass_subscript: Option<SsizeObjArgFunction>,
}

/// `Py_tp_getset` (`typeslots.h`).
pub const Py_tp_getset: c_int = 73;
/// `Py_mp_length` (`typeslots.h`).
pub const Py_mp_length: c_int = 4;
/// `Py_mp_subscript` (`typeslots.h`).
pub const Py_mp_subscript: c_int = 5;
/// `Py_mp_ass_subscript` (`typeslots.h`).
pub const Py_mp_ass_subscript: c_int = 3;
/// `Py_sq_length` (`typeslots.h`).
pub const Py_sq_length: c_int = 45;
/// `Py_sq_item` (`typeslots.h`).
pub const Py_sq_item: c_int = 44;
/// `Py_sq_ass_item` (`typeslots.h`).
pub const Py_sq_ass_item: c_int = 39;

/// `PyMethodDef`, the descriptor a `PyCFunction` is created from.
#[repr(C)]
pub struct PyMethodDef {
    /// `ml_name`.
    pub ml_name: *const c_char,
    /// `ml_meth`.
    pub ml_meth: Option<PyCFunction>,
    /// `ml_flags`.
    pub ml_flags: c_int,
    /// `ml_doc`.
    pub ml_doc: *const c_char,
}

impl PyMethodDef {
    /// The sentinel entry that ends a method table, the zeroed
    /// `PyMethodDef` a `PyMethodDef[]` array must be terminated with.
    pub const END: PyMethodDef = PyMethodDef {
        ml_name: std::ptr::null(),
        ml_meth: None,
        ml_flags: 0,
        ml_doc: std::ptr::null(),
    };
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

/// The unicode form of a byte string that carries no terminator. Rust
/// strings are length-delimited, so handing their pointer to
/// `PyUnicode_FromString` reads past the allocation; the length must cross
/// with the bytes.
pub unsafe fn unicode_from_bytes(bytes: &[u8]) -> *mut PyObject {
    // SAFETY: the bytes stay valid for the call's duration, and the C API
    // copies them.
    unsafe { PyUnicode_FromStringAndSize(bytes.as_ptr().cast(), bytes.len() as PySsize) }
}

unsafe extern "C" {
    pub static mut _Py_NoneStruct: PyObject;
    pub static mut _Py_TrueStruct: PyObject;
    pub static mut _Py_FalseStruct: PyObject;
    pub static mut Py_FrozenFlag: c_int;
    pub static mut Py_NoSiteFlag: c_int;
    pub static mut Py_IgnoreEnvironmentFlag: c_int;
    pub static mut PyModule_Type: PyTypeObject;
    pub static mut PyTraceBack_Type: PyTypeObject;
    pub static mut PyFloat_Type: PyTypeObject;
    pub static mut PyLong_Type: PyTypeObject;
    pub static mut PyTuple_Type: PyTypeObject;
    pub static mut PyList_Type: PyTypeObject;
    pub static mut PyBytes_Type: PyTypeObject;
    pub static mut PyUnicode_Type: PyTypeObject;
    pub static mut PyExc_ValueError: *mut PyObject;
    pub static mut _Py_NotImplementedStruct: PyObject;
}

unsafe extern "C" {
    pub fn Py_IsInitialized() -> c_int;
    pub fn Py_Initialize();
    pub fn Py_Finalize();
    pub fn Py_DecRef(obj: *mut PyObject);
    pub fn Py_IncRef(obj: *mut PyObject);
    pub fn PyEval_RestoreThread(tstate: *mut std::os::raw::c_void);
    pub fn PyEval_SaveThread() -> *mut std::os::raw::c_void;
    pub fn PyThreadState_Swap(tstate: *mut std::os::raw::c_void) -> *mut std::os::raw::c_void;
    pub fn PyRun_SimpleString(string: *const c_char) -> c_int;
    pub fn PySys_SetObject(name: *const c_char, value: *mut PyObject) -> c_int;
    pub fn PySys_GetObject(name: *const c_char) -> *mut PyObject;
    pub fn PyList_New(size: PySsize) -> *mut PyObject;
    pub fn PyList_SetItem(list: *mut PyObject, index: PySsize, item: *mut PyObject) -> c_int;
    pub fn PyUnicode_FromString(string: *const c_char) -> *mut PyObject;
    pub fn PyUnicode_FromStringAndSize(string: *const c_char, length: PySsize) -> *mut PyObject;
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
    pub fn PyUnicode_FromFormat(format: *const c_char, ...) -> *mut PyObject;
    pub fn PyErr_SetObject(exception: *mut PyObject, value: *mut PyObject) -> c_int;
    pub fn PyErr_SetString(exception: *mut PyObject, message: *const c_char);
    pub fn PyThreadState_Get() -> *mut std::os::raw::c_void;
    pub fn PyGILState_Check() -> c_int;
    pub fn PyImport_AddModule(name: *const c_char) -> *mut PyObject;
    pub fn PyImport_ImportModule(name: *const c_char) -> *mut PyObject;
    pub fn PyObject_Str(obj: *mut PyObject) -> *mut PyObject;
    pub fn PyObject_GetAttrString(obj: *mut PyObject, attr_name: *const c_char) -> *mut PyObject;
    pub fn PyObject_HasAttrString(obj: *mut PyObject, attr_name: *const c_char) -> c_int;
    pub fn PyObject_SetAttrString(
        obj: *mut PyObject,
        attr_name: *const c_char,
        value: *mut PyObject,
    ) -> c_int;
    pub fn PyObject_CallNoArgs(func: *mut PyObject) -> *mut PyObject;
    pub fn PyObject_CallObject(func: *mut PyObject, args: *mut PyObject) -> *mut PyObject;
    pub fn PyCallable_Check(obj: *mut PyObject) -> c_int;
    pub fn PyObject_IsTrue(obj: *mut PyObject) -> c_int;
    pub fn PyModule_AddFunctions(module: *mut PyObject, functions: *const PyMethodDef) -> c_int;
    pub fn PyModule_AddObject(
        module: *mut PyObject,
        name: *const c_char,
        value: *mut PyObject,
    ) -> c_int;
    pub fn PyBool_FromLong(value: std::os::raw::c_long) -> *mut PyObject;
    pub fn PyCFunction_New(def: *mut PyMethodDef, self_: *mut PyObject) -> *mut PyObject;
    pub fn PyArg_ParseTuple(args: *mut PyObject, format: *const c_char, ...) -> c_int;
    pub fn PyTuple_GetItem(tuple: *mut PyObject, index: PySsize) -> *mut PyObject;
    pub fn PyObject_Repr(obj: *mut PyObject) -> *mut PyObject;
    pub fn PyErr_Display(type_: *mut PyObject, value: *mut PyObject, traceback: *mut PyObject);
    pub fn PySys_WriteStderr(format: *const c_char, ...);
    pub fn PyCapsule_New(
        pointer: *mut std::os::raw::c_void,
        name: *const c_char,
        destructor: Option<PyCapsuleDestructor>,
    ) -> *mut PyObject;
    pub fn PyCapsule_GetPointer(
        capsule: *mut PyObject,
        name: *const c_char,
    ) -> *mut std::os::raw::c_void;
    pub fn PyCapsule_IsValid(capsule: *mut PyObject, name: *const c_char) -> c_int;
    pub static mut PyExc_TypeError: *mut PyObject;
    pub static mut PyExc_IndexError: *mut PyObject;
    pub static mut PyExc_AttributeError: *mut PyObject;
    pub static mut PyExc_EnvironmentError: *mut PyObject;
    pub static mut PyExc_IOError: *mut PyObject;
    pub static mut PyExc_SystemError: *mut PyObject;
    pub fn PyIndex_Check(obj: *mut PyObject) -> c_int;
    pub fn PyNumber_AsSsize_t(obj: *mut PyObject, exception: *mut PyObject) -> PySsize;
    pub fn PyType_FromSpec(spec: *mut PyTypeSpec) -> *mut PyObject;
    pub fn PyType_GenericAlloc(type_: *mut PyTypeObject, items: PySsize) -> *mut PyObject;
    pub fn PyType_GetName(type_: *mut PyTypeObject) -> *mut PyObject;
    pub fn PyObject_GetAttr(obj: *mut PyObject, attr_name: *mut PyObject) -> *mut PyObject;
    pub fn PyObject_GenericGetAttr(obj: *mut PyObject, name: *mut PyObject) -> *mut PyObject;
    pub fn PyObject_GenericSetAttr(
        obj: *mut PyObject,
        name: *mut PyObject,
        value: *mut PyObject,
    ) -> c_int;
    pub fn PyLong_FromLong(value: std::os::raw::c_long) -> *mut PyObject;
    pub fn PyLong_FromUnsignedLong(value: std::os::raw::c_ulong) -> *mut PyObject;
    pub fn PyLong_AsLong(obj: *mut PyObject) -> std::os::raw::c_long;
    pub fn PyFloat_AsDouble(obj: *mut PyObject) -> f64;
    pub fn PyFloat_FromDouble(value: f64) -> *mut PyObject;
    pub fn PySequence_Fast(obj: *mut PyObject, message: *const c_char) -> *mut PyObject;
    pub fn PySequence_Check(obj: *mut PyObject) -> c_int;
    pub fn PySequence_Size(obj: *mut PyObject) -> PySsize;
    pub fn PySequence_GetItem(obj: *mut PyObject, index: PySsize) -> *mut PyObject;
    pub fn PyTuple_Size(tuple: *mut PyObject) -> PySsize;
    pub fn PyTuple_New(size: PySsize) -> *mut PyObject;
    pub fn PyTuple_SetItem(tuple: *mut PyObject, index: PySsize, item: *mut PyObject) -> c_int;
    pub fn PyList_AsTuple(list: *mut PyObject) -> *mut PyObject;
    pub fn PyBytes_FromStringAndSize(string: *const c_char, length: PySsize) -> *mut PyObject;
    pub fn PyBytes_AsStringAndSize(
        obj: *mut PyObject,
        buffer: *mut *mut c_char,
        length: *mut PySsize,
    ) -> c_int;
    pub fn PyObject_Free(pointer: *mut std::os::raw::c_void);
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

    /// `PyTraceBack_Check` on a null result is `false`, never a dereference:
    /// `printTraceBack` receives whatever the caller passes as the traceback
    /// and must fall back to `PyErr_Display` for anything else.
    #[test]
    fn traceback_check_rejects_null() {
        assert!(!unsafe { py_traceback_check(std::ptr::null_mut()) });
    }

    /// The static `PyMethodDef` descriptors the host builds are `repr(C)`:
    /// name pointer, function pointer, `c_int` flags (padded to a word) and
    /// doc pointer, so 32 bytes on a 64-bit host. A compile-time size check
    /// keeps a CPython revision from silently mis-parsing the struct.
    #[test]
    fn the_method_def_layout_is_stable() {
        assert_eq!(
            std::mem::size_of::<PyMethodDef>(),
            4 * std::mem::size_of::<usize>()
        );
    }

    /// `refcount` on a null result is zero, never a dereference.
    #[test]
    fn refcount_rejects_null() {
        assert_eq!(unsafe { refcount(std::ptr::null_mut()) }, 0);
    }

    /// The descriptor structs the host assembles are `repr(C)` pointer
    /// tables: five words for `PyGetSetDef`, two for a slot, and the spec
    /// header carries the slot pointer last. A CPython revision that shifts
    /// one of these breaks the size check here, not at runtime.
    #[test]
    fn the_binding_struct_layouts_are_stable() {
        assert_eq!(
            std::mem::size_of::<PyGetSetDef>(),
            5 * std::mem::size_of::<usize>()
        );
        assert_eq!(
            std::mem::size_of::<PyTypeSlot>(),
            2 * std::mem::size_of::<usize>()
        );
        // The spec header packs `basicsize`, `itemsize` and `flags` into
        // two words between the name and the slot pointer.
        assert_eq!(
            std::mem::size_of::<PyTypeSpec>(),
            4 * std::mem::size_of::<usize>()
        );
        // The mapping table the port's sequence bridge lives in is three
        // function pointers wide; a revision that grows it breaks the
        // slot wiring, so the size is asserted.
        assert_eq!(
            std::mem::size_of::<PyMappingMethods>(),
            3 * std::mem::size_of::<usize>()
        );
    }
}
