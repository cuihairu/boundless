//! Attribute and method binding for native wrapper types.
//!
//! Reference: every exposed native class is a `PyObjectPlus` subclass whose
//! `PY_TYPEOBJECT` macro declares one `static PyTypeObject s_type_` for the
//! whole process (`lib/pyscript/pyobject_plus.hpp:116`) -- one class name is
//! one type object, forever. The protocol slots that macro fills in are the
//! generic ones: `pyGetAttribute` is `PyObject_GenericGetAttr` and
//! `pySetAttribute` is `PyObject_GenericSetAttr` (a `NULL` value deletes, in
//! `pyobject_plus.cpp:89-113`), `pyRepr` renders `"%s at %p"` (`:101`), and
//! `_pyNew` refuses direct construction with
//! `"%s() Cannot directly construct objects of this type"` (`:150`). Method
//! tables carry `METH_VARARGS | METH_KEYWORDS` handlers that parse with
//! `PyArg_ParseTuple` (`pyobject_plus.hpp:889-908`); a parse failure raises
//! the agreed `TypeError` from the API itself.
//!
//! The port builds the same shape as a heap type through `PyType_FromSpec`
//! instead of a static `PyTypeObject`: Rust has no C designated
//! initialiser, and the spec's slot table is assembled from a builder. The
//! static-storage equivalence holds by leaking the descriptor arrays (the
//! `s_type_` macro's storage never dies either) and by holding the one type
//! object per name in a registry keyed like the static itself. Methods are
//! bound with `METH_VARARGS` alone -- the port's handler type takes no
//! keyword dict -- and that difference is a registered platform gap.

use std::collections::BTreeMap;
use std::ffi::{CStr, CString};
use std::sync::{Mutex, MutexGuard, OnceLock};

use crate::capi;
use crate::pyref::PyRef;

/// Why a type could not be created.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BindError {
    /// A name carries an embedded NUL: it cannot become a C string.
    BadName,
    /// No interpreter is up: `PyType_FromSpec` is a C API call.
    NoInterpreter,
    /// `PyType_FromSpec` returned null; the pending error is cleared.
    CreateFailed,
}

/// One attribute of a native type: a getter, and a setter unless the
/// attribute is read-only.
struct Attribute {
    name: String,
    get: capi::Getter,
    set: Option<capi::Setter>,
}

/// One method of a native type: a `METH_VARARGS` handler.
struct Method {
    name: String,
    handler: capi::PyCFunction,
}

/// The description of one native type, the builder-side stand-in for the
/// reference's `PY_TYPEOBJECT` macro body.
///
/// Every attribute goes through the generic protocol (`pyGetAttribute` /
/// `pySetAttribute`), so a native attribute is a getset descriptor and
/// nothing else; methods are bound as `METH_VARARGS` handlers, exactly the
/// method-table shape of `pyobject_plus.hpp:889-908` minus the keyword
/// argument flag.
pub struct TypeSpec {
    name: String,
    attributes: Vec<Attribute>,
    methods: Vec<Method>,
}

impl TypeSpec {
    /// A type description with a dotted name and no members yet.
    pub fn new(name: &str) -> Self {
        Self {
            name: name.to_owned(),
            attributes: Vec::new(),
            methods: Vec::new(),
        }
    }

    /// Add an attribute: `getter` is called for every read, and `setter` is
    /// called for every assignment unless the attribute is read-only.
    #[must_use]
    pub fn attribute(
        mut self,
        name: &str,
        getter: capi::Getter,
        setter: Option<capi::Setter>,
    ) -> Self {
        self.attributes.push(Attribute {
            name: name.to_owned(),
            get: getter,
            set: setter,
        });
        self
    }

    /// Add a `METH_VARARGS` method: `handler` receives `(self, args)` and
    /// parses the tuple itself, the way the reference's method tables do.
    #[must_use]
    pub fn method(mut self, name: &str, handler: capi::PyCFunction) -> Self {
        self.methods.push(Method {
            name: name.to_owned(),
            handler,
        });
        self
    }
}

/// One name's type object, the registry the static `s_type_` declarations
/// stand in for: the name maps to the type object, and the type object maps
/// back to its dotted name for the `pyRepr` / `_pyNew` text. Addresses are
/// stored as `usize` so the registry stays `Send`, the same shape as the
/// identity cache.
struct Registry {
    types: BTreeMap<String, usize>,
    names: BTreeMap<usize, String>,
}

fn registry() -> MutexGuard<'static, Registry> {
    static REGISTRY: OnceLock<Mutex<Registry>> = OnceLock::new();
    REGISTRY
        .get_or_init(|| {
            Mutex::new(Registry {
                types: BTreeMap::new(),
                names: BTreeMap::new(),
            })
        })
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// A name as a NUL-terminated byte string, or `None` when it carries an
/// embedded NUL and cannot cross into the C API at all.
fn to_c_string(text: &str) -> Option<CString> {
    CString::new(text).ok()
}

/// The dotted name of a type object: the registry's own for a type this
/// module created (the `tp_name` of the reference's `s_type_`), the C API's
/// `PyType_GetName` for anything else.
///
/// # Safety
///
/// `type_` must be a type pointer CPython handed out, held while the
/// interpreter is up.
pub unsafe fn qualified_name(type_: *mut capi::PyTypeObject) -> String {
    {
        let registry = registry();
        if let Some(name) = registry.names.get(&(type_ as usize)) {
            return name.clone();
        }
    }
    // A foreign type: ask the C API, whose answer is a strong reference.
    // SAFETY: `type_` is a type pointer CPython handed out.
    unsafe {
        let name = capi::PyType_GetName(type_);
        let text = capi::to_string(name);
        capi::dec_ref(name);
        text
    }
}

/// `_pyNew` (`pyobject_plus.cpp:150`): constructing one of these from a
/// script is refused, the engine builds them instead. The message is the
/// reference's, with the dotted type name in the `%s`.
///
/// SAFETY: CPython calls this with the type object it is constructing.
unsafe extern "C" fn refuse_new(
    type_: *mut capi::PyTypeObject,
    _args: *mut capi::PyObject,
    _kwds: *mut capi::PyObject,
) -> *mut capi::PyObject {
    // SAFETY: the refusal only formats the name and sets the error.
    unsafe {
        let name = qualified_name(type_);
        let message = capi::PyUnicode_FromFormat(
            c"%U() Cannot directly construct objects of this type".as_ptr(),
            capi::PyUnicode_FromString(name.as_ptr().cast()),
        );
        // A refused construction always returns null with the error pending;
        // any other return makes `type_call` raise `SystemError` for
        // "returned a result with an exception set".
        capi::PyErr_SetObject(capi::PyExc_TypeError, message);
        capi::dec_ref(message);
        std::ptr::null_mut()
    }
}

/// `pyRepr` (`pyobject_plus.cpp:101`): `"%s at %p"`, the dotted type name
/// and the instance address. The Windows spelling adds the `0x` prefix
/// itself (`"%s at 0x%p"`); only the non-Windows format is implemented, a
/// registered platform gap.
///
/// SAFETY: CPython calls this with one of the module's own instances.
unsafe extern "C" fn default_repr(object: *mut capi::PyObject) -> *mut capi::PyObject {
    // SAFETY: the repr only reads the type name and formats a new string.
    unsafe {
        let name = qualified_name(capi::Py_TYPE(object));
        let rendered = format!("{name} at 0x{:x}", object as usize);
        capi::PyUnicode_FromString(rendered.as_ptr().cast())
    }
}

/// Create the type object a name describes, or hand back the one that
/// already exists for the name -- one name is one type object for the
/// process, the `s_type_` rule.
///
/// The returned handle owns one strong reference; the registry holds its
/// own, so the type object survives every caller's handle.
pub fn create_type(spec: &TypeSpec) -> Result<PyRef, BindError> {
    // Every name crosses into C as a NUL-terminated byte string; one that
    // cannot stops the whole spec.
    let mut names: Vec<&'static CStr> = Vec::new();
    let mut push = |text: &str| -> Result<(), BindError> {
        let encoded = to_c_string(text).ok_or(BindError::BadName)?;
        names.push(Box::leak(encoded.into_boxed_c_str()));
        Ok(())
    };
    push(&spec.name)?;
    for attribute in &spec.attributes {
        push(&attribute.name)?;
    }
    for method in &spec.methods {
        push(&method.name)?;
    }

    let mut registry = registry();
    if let Some(&existing) = registry.types.get(&spec.name) {
        // The static `s_type_` is already up: the caller gets a fresh
        // reference of the one type object, the `Py_XINCREF`-and-adopt of
        // the identity cache's existing path.
        let pointer = existing as *mut capi::PyObject;
        // SAFETY: the registry only stores objects this function created.
        unsafe { capi::inc_ref(pointer) };
        return Ok(PyRef::borrowed(pointer));
    }
    if unsafe { capi::Py_IsInitialized() } == 0 {
        return Err(BindError::NoInterpreter);
    }

    // The method table, terminated by a null name the way the reference's
    // macro-generated tables are.
    let mut methods: Vec<capi::PyMethodDef> = spec
        .methods
        .iter()
        .zip(&names[1 + spec.attributes.len()..])
        .map(|(method, name)| capi::PyMethodDef {
            ml_name: name.as_ptr(),
            ml_meth: Some(method.handler),
            ml_flags: capi::METH_VARARGS,
            ml_doc: std::ptr::null(),
        })
        .collect();
    methods.push(capi::PyMethodDef {
        ml_name: std::ptr::null(),
        ml_meth: None,
        ml_flags: 0,
        ml_doc: std::ptr::null(),
    });
    let methods: &'static mut [capi::PyMethodDef] = Box::leak(methods.into_boxed_slice());

    // The attribute table: one getset descriptor per attribute, terminated
    // by a null name. A missing setter is the read-only attribute.
    let mut attributes: Vec<capi::PyGetSetDef> = spec
        .attributes
        .iter()
        .zip(&names[1..1 + spec.attributes.len()])
        .map(|(attribute, name)| capi::PyGetSetDef {
            name: name.as_ptr(),
            get: Some(attribute.get),
            set: attribute.set,
            doc: std::ptr::null(),
            closure: std::ptr::null_mut(),
        })
        .collect();
    attributes.push(capi::PyGetSetDef {
        name: std::ptr::null(),
        get: None,
        set: None,
        doc: std::ptr::null(),
        closure: std::ptr::null_mut(),
    });
    let attributes: &'static mut [capi::PyGetSetDef] = Box::leak(attributes.into_boxed_slice());

    // The protocol slots the reference fills in with the generic handlers:
    // `pyGetAttribute`/`pySetAttribute` are `PyObject_GenericGetAttr`/
    // `PyObject_GenericSetAttr` (a `NULL` value in the slot layer is the
    // delete, which `PyObject_GenericSetAttr` itself handles), `_pyNew` the
    // refusal, `pyRepr` the address format.
    let slots: &'static mut [capi::PyTypeSlot] = Box::leak(
        vec![
            capi::PyTypeSlot {
                slot: capi::Py_tp_getattro,
                pfunc: capi::PyObject_GenericGetAttr as *mut std::os::raw::c_void,
            },
            capi::PyTypeSlot {
                slot: capi::Py_tp_setattro,
                pfunc: capi::PyObject_GenericSetAttr as *mut std::os::raw::c_void,
            },
            capi::PyTypeSlot {
                slot: capi::Py_tp_new,
                pfunc: refuse_new as *mut std::os::raw::c_void,
            },
            capi::PyTypeSlot {
                slot: capi::Py_tp_repr,
                pfunc: default_repr as *mut std::os::raw::c_void,
            },
            capi::PyTypeSlot {
                slot: capi::Py_tp_methods,
                pfunc: methods.as_mut_ptr().cast(),
            },
            capi::PyTypeSlot {
                slot: capi::Py_tp_getset,
                pfunc: attributes.as_mut_ptr().cast(),
            },
            capi::PyTypeSlot {
                slot: 0,
                pfunc: std::ptr::null_mut(),
            },
        ]
        .into_boxed_slice(),
    );

    // The instance body is the bare `PyObject` header: the port's wrapper
    // state lives in the identity cache, not in the instance, so the base
    // size is the whole size.
    let type_spec: &'static mut capi::PyTypeSpec = Box::leak(Box::new(capi::PyTypeSpec {
        name: names[0].as_ptr(),
        basicsize: std::mem::size_of::<capi::PyObject>() as std::os::raw::c_int,
        itemsize: 0,
        flags: 0,
        slots: slots.as_mut_ptr(),
    }));

    // The descriptor arrays and the spec are leaked above on purpose: the
    // type object keeps pointing into them, the same way the reference's
    // static `s_type_` outlives its own macro-generated tables.
    // SAFETY: the interpreter is up and this thread owns the GIL.
    let object = unsafe { capi::PyType_FromSpec(type_spec) };
    let Some(object) = PyRef::from_owned(object) else {
        // SAFETY: the failure left a pending exception.
        unsafe { capi::clear_error() };
        return Err(BindError::CreateFailed);
    };

    // The registry takes its own reference, the static's immortality: the
    // one type object per name outlives every caller's handle.
    let cached = object.clone();
    let address = cached.as_ptr() as usize;
    registry.types.insert(spec.name.clone(), address);
    registry.names.insert(address, spec.name.clone());
    std::mem::forget(cached);
    Ok(object)
}

/// The engine's construction path: allocate one instance of a native type
/// directly, the way the reference's C++ `new` builds a `PyObjectPlus`
/// without ever passing through the script-facing `_pyNew`. Script calls go
/// through `tp_new` and are refused; the host comes here.
///
/// # Safety
///
/// `type_object` must be a handle to a type this module created, held while
/// the interpreter is up on the calling thread.
pub unsafe fn instantiate(type_object: &PyRef) -> Option<PyRef> {
    if type_object.as_ptr().is_null() || unsafe { capi::Py_IsInitialized() } == 0 {
        return None;
    }
    // SAFETY: the interpreter is up and the handle is one of this module's
    // type objects.
    let instance = unsafe { capi::PyType_GenericAlloc(type_object.as_ptr().cast(), 0) };
    PyRef::from_owned(instance)
}

/// `Interpreter::fini`'s step for this module: release the registry's
/// reference of every type object while the allocator is still alive, and
/// start the next interpreter with an empty registry. The leaked descriptor
/// arrays stay behind, the static-storage rule.
pub fn reset() {
    let mut registry = registry();
    let types = std::mem::take(&mut registry.types);
    registry.names.clear();
    for (_, address) in types {
        let pointer = address as *mut capi::PyObject;
        // `Interpreter::fini` runs this before `Py_Finalize`, so the
        // allocator the reference counts into is still alive.
        // SAFETY: every stored pointer came out of `PyType_FromSpec`.
        unsafe { capi::dec_ref(pointer) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Creating a type is a C API call: without an interpreter there is no
    /// type object to build and the registry stays empty.
    #[test]
    fn create_type_refuses_without_an_interpreter() {
        let spec = TypeSpec::new("boundless.Refused");
        assert_eq!(create_type(&spec), Err(BindError::NoInterpreter));
        assert!(registry().types.is_empty());
    }

    /// A name with an embedded NUL cannot become a C string and stops the
    /// whole spec before any C API call.
    #[test]
    fn a_bad_name_stops_the_spec() {
        let spec = TypeSpec::new("boundless.Demo").attribute("va\0lue", value_get, None);
        assert_eq!(create_type(&spec), Err(BindError::BadName));
        assert!(registry().types.is_empty());
    }

    /// The empty handle instantiates nothing, whatever the interpreter
    /// state: there is no type object to allocate from.
    #[test]
    fn the_empty_handle_instantiates_nothing() {
        // SAFETY: the empty handle carries no pointer to misuse.
        assert!(unsafe { instantiate(&PyRef::borrowed(std::ptr::null_mut())) }.is_none());
    }

    /// A poisoned registry lock recovers for the next caller, like the
    /// other process-wide registries.
    #[test]
    fn a_poisoned_registry_lock_recovers() {
        let poisoner = std::thread::spawn(|| {
            let _guard = registry();
            panic!("poison the registry lock");
        });
        assert!(poisoner.join().is_err());
        assert!(registry().types.is_empty());
    }

    /// Shared by the bad-name test: any getter signature will do, the spec
    /// is refused before it is used.
    ///
    /// Called on its own below: the bad-name spec never reaches it, so its
    /// null return is exercised directly.
    #[test]
    fn the_stub_getter_returns_nothing() {
        // SAFETY: the stub ignores both arguments and returns a null
        // pointer without touching either.
        let result = unsafe { value_get(std::ptr::null_mut(), std::ptr::null_mut()) };
        assert!(result.is_null());
    }

    unsafe extern "C" fn value_get(
        _self_: *mut capi::PyObject,
        _closure: *mut std::os::raw::c_void,
    ) -> *mut capi::PyObject {
        std::ptr::null_mut()
    }
}
