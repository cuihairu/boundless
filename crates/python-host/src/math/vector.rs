//! The `Math.Vector2` / `Math.Vector3` / `Math.Vector4` classes, the
//! reference's `PyVector<V>` template (`lib/pyscript/script_math.cpp:1161-2501`).
//!
//! Every dimension is one real type object whose `tp_new` builds a fresh
//! `PyVectorCopy` -- zeroed, or filled through the `py_set` forms when
//! arguments are given (`script_math.cpp:1276-1298`; keyword arguments are
//! accepted and ignored). The whole operator set is per-call: add/subtract
//! parse *both* operands through `Script::setData`, so a tuple works on
//! either side (`:1405-1440`); multiply specialises for Vector3 into the
//! cross product (`:1478-1519`); divide is left-vector right-scalar only
//! (`:1442-1458`); every in-place operator mutates through
//! `safeSetVector` -- the read-only wall -- and hands the same object back
//! (`:1566-1655`). Comparisons are the migrated rich compare: same-width
//! only, lexicographic (`:2445-2501`). The instance body is four floats
//! plus the read-only flag (`script_math.hpp:340-360`).

use std::collections::BTreeMap;
use std::sync::{Mutex, OnceLock};

use crate::binding::{self, TypeSpec};
use crate::capi;
use crate::pyref::PyRef;

use super::{
    body, bool_object, check_type, coerce_float, cpp_float, float_object, own_raw,
    raise_index_error, raise_type_error, remember_type, takes_no_arguments, type_address,
};

/// The instance body: the four component slots (only `dim` are live) and
/// the read-only wall (`script_math.hpp:360`).
pub(super) struct VectorBody {
    pub values: [f32; 4],
    pub read_only: bool,
}

/// Which dimension each created vector type serves, keyed on the type
/// object's address: the `self`-shaped handlers are shared between the
/// dimensions and recover theirs from the instance's type.
static DIMS: OnceLock<Mutex<BTreeMap<usize, usize>>> = OnceLock::new();

fn dims() -> &'static Mutex<BTreeMap<usize, usize>> {
    DIMS.get_or_init(|| Mutex::new(BTreeMap::new()))
}

/// The Python name of one dimension's type.
fn vector_name(dim: usize) -> &'static str {
    match dim {
        2 => "Math.Vector2",
        3 => "Math.Vector3",
        _ => "Math.Vector4",
    }
}

/// The dimension a vector instance serves.
///
/// # Safety
///
/// `object` must be an instance of a vector type this module created.
unsafe fn dim_of(object: *mut capi::PyObject) -> Option<usize> {
    let address = unsafe { capi::Py_TYPE(object) } as usize;
    dims()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(&address)
        .copied()
}

/// The components of a vector instance.
///
/// # Safety
///
/// `object` must be an instance of a vector type of this module.
pub(crate) unsafe fn values_of(object: *mut capi::PyObject) -> [f32; 4] {
    // SAFETY: the caller guarantees the instance shape.
    unsafe { (*body::<VectorBody>(object)).values }
}

/// A fresh writable copy of a component set, the `Script::getData(Vector&)`
/// of every operator and getter result (`script.cpp:2311-2332`).
///
/// # Safety
///
/// The interpreter must be up on the calling thread, with the vector types
/// of this module installed.
pub(crate) unsafe fn vector_object(values: [f32; 4], dim: usize) -> PyRef {
    let address = type_address(vector_name(dim)).expect("the vector type is installed");
    let type_object = PyRef::borrowed(address as *mut capi::PyObject);
    // SAFETY: the type is one of this module's, and the interpreter is up.
    let instance = unsafe { binding::instantiate(&type_object) }
        .expect("the vector instance allocation cannot fail");
    // SAFETY: the instance is the fresh allocation this call owns.
    unsafe {
        (*body::<VectorBody>(instance.as_ptr())).values = values;
    }
    instance
}

/// `PyVector<V>::safeSetVector` (`script_math.cpp:1256-1268`): the
/// read-only wall, then the write.
///
/// # Safety
///
/// `object` must be an instance of a vector type of this module.
unsafe fn safe_set_vector(object: *mut capi::PyObject, values: [f32; 4]) -> bool {
    // SAFETY: the caller guarantees the instance shape.
    let slot = unsafe { body::<VectorBody>(object) };
    if unsafe { (*slot).read_only } {
        raise_type_error("Vector is read-only");
        return false;
    }
    unsafe { (*slot).values = values };
    true
}

/// One component as a fresh float object, the `Script::getData(float&)` of
/// the indexed reads.
fn component_object(values: [f32; 4], index: usize) -> *mut capi::PyObject {
    float_object(values[index] as f64)
}

/// The `PyArg_ParseTuple( pObject, "fff...f" )` of `Script::setData`: a
/// tuple of exactly `dim` number items (`script.cpp:1868-1880`).
///
/// # Safety
///
/// `object` must be a borrowed object the interpreter handed out, or null.
unsafe fn parse_float_tuple(object: *mut capi::PyObject, dim: usize) -> Option<[f32; 4]> {
    if !unsafe { capi::py_tuple_check(object) }
        || unsafe { capi::PyTuple_Size(object) } != dim as isize
    {
        return None;
    }
    let mut values = [0.0; 4];
    for (index, slot) in values.iter_mut().enumerate().take(dim) {
        // SAFETY: the size check above bounds the index.
        let item = unsafe { capi::PyTuple_GetItem(object, index as isize) };
        if !unsafe { capi::py_float_check(item) } && !unsafe { capi::py_long_check(item) } {
            // The "f" conversion of `PyArg_ParseTuple` leaves a `TypeError`
            // pending on a non-number; the caller either replaces the
            // message or clears it.
            unsafe { capi::clear_error() };
            return None;
        }
        // SAFETY: the checks above guarantee a number object.
        let value = unsafe { capi::PyFloat_AsDouble(item) };
        *slot = value as f32;
    }
    Some(values)
}

/// `Script::setData(PyObject*, VectorN&, varName)` (`script.cpp:1854-1935`):
/// a same-width vector instance or a tuple of `dim` floats; anything else
/// is the pending `TypeError` named after `var_name`.
///
/// # Safety
///
/// `object` must be a borrowed object the interpreter handed out, or null.
pub(crate) unsafe fn parse_vector_like(
    object: *mut capi::PyObject,
    dim: usize,
    var_name: &str,
) -> Option<[f32; 4]> {
    if unsafe { check_type(object, vector_name(dim)) } {
        // SAFETY: the check above guarantees the instance shape.
        return Some(unsafe { values_of(object) });
    }
    unsafe { capi::clear_error() };
    if let Some(values) = unsafe { parse_float_tuple(object, dim) } {
        return Some(values);
    }
    unsafe { capi::clear_error() };
    // The three dimensions spell the sentence differently: Vector2 and
    // Vector4 drop the article before "tuple" (`script.cpp:1869`, `:1885`,
    // `:1930`).
    let text = match dim {
        2 => format!("{var_name} must be set to a Vector2 or tuple of 2 floats"),
        3 => format!("{var_name} must be set to a Vector3 or a tuple of 3 floats"),
        _ => format!("{var_name} must be set to a Vector4 or tuple of 4 floats"),
    };
    raise_type_error(&text);
    None
}

/// The shared `py_set` parse (`script_math.cpp:1798-1860`): the argument
/// forms of the `set` method and the constructor. `None` means the caller's
/// fallback sentence is due; a failed float parse leaves its own error
/// pending for the caller to replace.
///
/// # Safety
///
/// `args` must be the argument tuple a C call handed over.
unsafe fn parse_set_form(args: *mut capi::PyObject, dim: usize) -> Option<[f32; 4]> {
    if unsafe { capi::PyTuple_Size(args) } != 1 {
        return unsafe { parse_float_tuple(args, dim) };
    }
    // SAFETY: the size check above bounds the index.
    let item = unsafe { capi::PyTuple_GetItem(args, 0) };
    if unsafe { capi::py_tuple_check(item) } {
        return unsafe { parse_float_tuple(item, dim) };
    }
    if unsafe { capi::py_list_check(item) } {
        // `PyList_AsTuple` copies the reference-counted items
        // (`script_math.cpp:1815-1816`), so the conversion can be dropped
        // as soon as the parse has run.
        // SAFETY: the check above guarantees a list object.
        let converted = PyRef::from_owned(unsafe { capi::PyList_AsTuple(item) })?;
        let parsed = unsafe { parse_float_tuple(converted.as_ptr(), dim) };
        drop(converted);
        return parsed;
    }
    if unsafe { check_type(item, vector_name(dim)) } {
        // SAFETY: the check above guarantees the instance shape.
        return Some(unsafe { values_of(item) });
    }
    // The single-float repeating form (`script_math.cpp:1826-1836`), with
    // the default empty variable name behind the parse.
    unsafe { coerce_float(item, "") }.map(|value| [value as f32; 4])
}

/// The final `py_set` sentence (`script_math.cpp:1849-1854`), always the
/// visible error however the parse failed.
fn raise_set_failed(dim: usize) {
    raise_type_error(&format!(
        "Vector.set must be set to a sequence of {dim} floats, or one float"
    ));
}

/// Build a fresh vector from constructor-style arguments -- the
/// `PyVector<V>::pyNew` of `dot` and `cross2D` (`script_math.cpp:1303-1324`):
/// no arguments is the zero vector, any parse failure raises the set
/// sentence.
///
/// # Safety
///
/// The interpreter is up; `args` is the argument tuple a C call handed over.
unsafe fn construct_from_args(args: *mut capi::PyObject, dim: usize) -> Option<PyRef> {
    let values = if unsafe { capi::PyTuple_Size(args) } == 0 {
        [0.0; 4]
    } else {
        match unsafe { parse_set_form(args, dim) } {
            Some(values) => values,
            None => {
                raise_set_failed(dim);
                return None;
            }
        }
    };
    // SAFETY: the interpreter is up and the type is installed.
    Some(unsafe { vector_object(values, dim) })
}

/// The real constructor bodies, one per dimension so the type object the
/// allocation comes from is the one installed for that dimension.
macro_rules! vector_new_for {
    ($name:ident, $dim:expr) => {
        /// # Safety
        ///
        /// A C `tp_new` call: the interpreter holds the GIL.
        unsafe extern "C" fn $name(
            type_object: *mut capi::PyTypeObject,
            args: *mut capi::PyObject,
            _kwargs: *mut capi::PyObject,
        ) -> *mut capi::PyObject {
            let handle = PyRef::borrowed(type_object.cast());
            // SAFETY: the type is one of this module's and the interpreter
            // is up.
            let Some(instance) = (unsafe { binding::instantiate(&handle) }) else {
                raise_type_error("Vector allocation failed");
                return std::ptr::null_mut();
            };
            if unsafe { capi::PyTuple_Size(args) } > 0 {
                let parsed = unsafe { parse_set_form(args, $dim) };
                let Some(values) = parsed else {
                    raise_set_failed($dim);
                    return std::ptr::null_mut();
                };
                // SAFETY: the instance is the fresh copy this call built.
                unsafe {
                    (*body::<VectorBody>(instance.as_ptr())).values = values;
                }
            }
            own_raw(instance)
        }
    };
}

vector_new_for!(vector2_new, 2);
vector_new_for!(vector3_new, 3);
vector_new_for!(vector4_new, 4);

/// `PyVector<V>::_py_add` / `_py_subtract` (`script_math.cpp:1405-1440`):
/// both operands through `setData` -- a tuple works on either side -- else
/// the pending error clears and `NotImplemented` answers.
///
/// # Safety
///
/// A C number-slot call; the interpreter holds the GIL.
unsafe fn add_or_subtract(
    a: *mut capi::PyObject,
    b: *mut capi::PyObject,
    dim: usize,
    add: bool,
) -> *mut capi::PyObject {
    let (Some(a_values), Some(b_values)) = (unsafe { parse_vector_like(a, dim, "") }, unsafe {
        parse_vector_like(b, dim, "")
    }) else {
        // A failed parse leaves a pending error that the `NotImplemented`
        // path clears (`script_math.cpp:1434-1439`).
        unsafe { capi::clear_error() };
        return capi::py_not_implemented();
    };
    let mut values = [0.0; 4];
    for index in 0..4 {
        values[index] = if add {
            a_values[index] + b_values[index]
        } else {
            a_values[index] - b_values[index]
        };
    }
    // SAFETY: the interpreter is up and the types are installed.
    own_raw(unsafe { vector_object(values, dim) })
}

/// `PyVector<V>::_py_multiply` (`script_math.cpp:1461-1519`): scalar
/// multiplication from either side; the Vector3 specialisation turns two
/// vectors into the cross product.
///
/// # Safety
///
/// A C number-slot call; the interpreter holds the GIL.
unsafe fn multiply(
    a: *mut capi::PyObject,
    b: *mut capi::PyObject,
    dim: usize,
) -> *mut capi::PyObject {
    let a_vector = unsafe { check_type(a, vector_name(dim)) };
    let b_vector = unsafe { check_type(b, vector_name(dim)) };
    if a_vector && b_vector {
        if dim == 3 {
            // SAFETY: the checks above guarantee the instance shapes.
            let x = unsafe { values_of(a) };
            let y = unsafe { values_of(b) };
            let values = [
                x[1] * y[2] - x[2] * y[1],
                x[2] * y[0] - x[0] * y[2],
                x[0] * y[1] - x[1] * y[0],
                0.0,
            ];
            return own_raw(unsafe { vector_object(values, dim) });
        }
        // Two vectors of widths 2 or 4 never multiply.
        unsafe { capi::clear_error() };
        return capi::py_not_implemented();
    }
    let Some(factor) = (if a_vector {
        unsafe { coerce_float(b, "") }
    } else if b_vector {
        unsafe { coerce_float(a, "") }
    } else {
        None
    }) else {
        unsafe { capi::clear_error() };
        return capi::py_not_implemented();
    };
    // SAFETY: a check above guarantees the instance shape.
    let values = if a_vector {
        unsafe { values_of(a) }
    } else {
        unsafe { values_of(b) }
    }
    .map(|component| component * factor as f32);
    own_raw(unsafe { vector_object(values, dim) })
}

/// `PyVector<V>::_py_divide` (`script_math.cpp:1442-1458`): a vector on
/// the left, a float on the right, nothing else.
///
/// # Safety
///
/// A C number-slot call; the interpreter holds the GIL.
unsafe fn divide(
    a: *mut capi::PyObject,
    b: *mut capi::PyObject,
    dim: usize,
) -> *mut capi::PyObject {
    let Some(factor) = (unsafe { check_type(a, vector_name(dim)) })
        .then(|| unsafe { coerce_float(b, "") })
        .flatten()
    else {
        unsafe { capi::clear_error() };
        return capi::py_not_implemented();
    };
    // SAFETY: the check above guarantees the instance shape.
    let values = unsafe { values_of(a) }.map(|component| component / factor as f32);
    own_raw(unsafe { vector_object(values, dim) })
}

/// `PyVector<V>::_py_negative` / `_py_positive` (`script_math.cpp:1521-1531`):
/// fresh negated copy, fresh copy.
///
/// # Safety
///
/// A C number-slot call; the interpreter holds the GIL.
unsafe fn negative(object: *mut capi::PyObject, dim: usize) -> *mut capi::PyObject {
    // SAFETY: the caller guarantees the instance shape.
    let values = unsafe { values_of(object) }.map(|component| -component);
    own_raw(unsafe { vector_object(values, dim) })
}

/// # Safety
///
/// A C number-slot call; the interpreter holds the GIL.
unsafe fn positive(object: *mut capi::PyObject, dim: usize) -> *mut capi::PyObject {
    // SAFETY: the caller guarantees the instance shape.
    let values = unsafe { values_of(object) };
    own_raw(unsafe { vector_object(values, dim) })
}

/// `PyVector<V>::_py_nonzero` (`script_math.cpp:1533-1537`): the length
/// squared decides the truth value.
///
/// # Safety
///
/// A C number-slot call; the interpreter holds the GIL.
unsafe fn nonzero(object: *mut capi::PyObject, dim: usize) -> capi::c_int {
    // SAFETY: the caller guarantees the instance shape.
    let values = unsafe { values_of(object) };
    let squared: f32 = (0..dim).map(|index| values[index] * values[index]).sum();
    (squared > 0.0) as capi::c_int
}

/// The in-place operators (`script_math.cpp:1566-1655`): parse the right
/// operand, mutate through `safeSetVector` -- the read-only wall raises --
/// and hand the same object back. Parse failures clear the pending error
/// and answer `NotImplemented`, the reference's own undo.
///
/// # Safety
///
/// A C number-slot call; the interpreter holds the GIL.
unsafe fn inplace_add_or_subtract(
    self_: *mut capi::PyObject,
    b: *mut capi::PyObject,
    dim: usize,
    add: bool,
) -> *mut capi::PyObject {
    let Some(b_values) = (unsafe { parse_vector_like(b, dim, "") }) else {
        unsafe { capi::clear_error() };
        return capi::py_not_implemented();
    };
    // SAFETY: the caller guarantees the instance shape.
    let values = unsafe { values_of(self_) };
    let mut result = [0.0; 4];
    for index in 0..4 {
        result[index] = if add {
            values[index] + b_values[index]
        } else {
            values[index] - b_values[index]
        };
    }
    if !unsafe { safe_set_vector(self_, result) } {
        return std::ptr::null_mut();
    }
    // SAFETY: the same-object answer carries its increment
    // (`script_math.cpp:1580-1583`).
    unsafe { capi::inc_ref(self_) };
    self_
}

/// The scalar in-place multiply/divide shared by the dimensions.
///
/// # Safety
///
/// A C number-slot call; the interpreter holds the GIL.
unsafe fn inplace_scalar(
    self_: *mut capi::PyObject,
    b: *mut capi::PyObject,
    multiply: bool,
) -> *mut capi::PyObject {
    let Some(factor) = (unsafe { coerce_float(b, "") }) else {
        unsafe { capi::clear_error() };
        return capi::py_not_implemented();
    };
    // SAFETY: the caller guarantees the instance shape.
    let values = unsafe { values_of(self_) };
    let result = values.map(|component| {
        if multiply {
            component * factor as f32
        } else {
            component / factor as f32
        }
    });
    if !unsafe { safe_set_vector(self_, result) } {
        return std::ptr::null_mut();
    }
    // SAFETY: the same-object answer carries its increment.
    unsafe { capi::inc_ref(self_) };
    self_
}

/// The Vector3 in-place multiply (`script_math.cpp:1614-1643`): a vector
/// operand is the cross product assignment, a scalar the plain multiply.
///
/// # Safety
///
/// A C number-slot call; the interpreter holds the GIL.
unsafe fn inplace_multiply3(
    self_: *mut capi::PyObject,
    b: *mut capi::PyObject,
) -> *mut capi::PyObject {
    if unsafe { check_type(b, "Math.Vector3") } {
        // SAFETY: the checks above guarantee the instance shapes.
        let x = unsafe { values_of(self_) };
        let y = unsafe { values_of(b) };
        let cross = [
            x[1] * y[2] - x[2] * y[1],
            x[2] * y[0] - x[0] * y[2],
            x[0] * y[1] - x[1] * y[0],
            0.0,
        ];
        if !unsafe { safe_set_vector(self_, cross) } {
            return std::ptr::null_mut();
        }
        unsafe { capi::inc_ref(self_) };
        return self_;
    }
    unsafe { inplace_scalar(self_, b, true) }
}

/// One static-dimension number-slot shim: the slot belongs to one type, so
/// the dimension is a compile-time constant and the left operand may be
/// anything at all.
macro_rules! number_for {
    ($name:ident, |$a:ident, $b:ident| $body:expr) => {
        /// # Safety
        ///
        /// A C number-slot call; the interpreter holds the GIL.
        unsafe extern "C" fn $name(
            $a: *mut capi::PyObject,
            $b: *mut capi::PyObject,
        ) -> *mut capi::PyObject {
            $body
        }
    };
    ($name:ident, |$s:ident| $body:expr) => {
        /// # Safety
        ///
        /// A C number-slot call; the interpreter holds the GIL.
        unsafe extern "C" fn $name($s: *mut capi::PyObject) -> *mut capi::PyObject {
            $body
        }
    };
    ($name:ident, |$s:ident| -> capi::c_int $body:expr) => {
        /// # Safety
        ///
        /// A C number-slot call; the interpreter holds the GIL.
        unsafe extern "C" fn $name($s: *mut capi::PyObject) -> capi::c_int {
            $body
        }
    };
}

number_for!(vector2_add, |a, b| unsafe {
    add_or_subtract(a, b, 2, true)
});
number_for!(vector2_subtract, |a, b| unsafe {
    add_or_subtract(a, b, 2, false)
});
number_for!(vector3_add, |a, b| unsafe {
    add_or_subtract(a, b, 3, true)
});
number_for!(vector3_subtract, |a, b| unsafe {
    add_or_subtract(a, b, 3, false)
});
number_for!(vector4_add, |a, b| unsafe {
    add_or_subtract(a, b, 4, true)
});
number_for!(vector4_subtract, |a, b| unsafe {
    add_or_subtract(a, b, 4, false)
});
number_for!(vector2_multiply, |a, b| unsafe { multiply(a, b, 2) });
number_for!(vector3_multiply, |a, b| unsafe { multiply(a, b, 3) });
number_for!(vector4_multiply, |a, b| unsafe { multiply(a, b, 4) });
number_for!(vector2_divide, |a, b| unsafe { divide(a, b, 2) });
number_for!(vector3_divide, |a, b| unsafe { divide(a, b, 3) });
number_for!(vector4_divide, |a, b| unsafe { divide(a, b, 4) });
number_for!(vector2_negative, |a| unsafe { negative(a, 2) });
number_for!(vector3_negative, |a| unsafe { negative(a, 3) });
number_for!(vector4_negative, |a| unsafe { negative(a, 4) });
number_for!(vector2_positive, |a| unsafe { positive(a, 2) });
number_for!(vector3_positive, |a| unsafe { positive(a, 3) });
number_for!(vector4_positive, |a| unsafe { positive(a, 4) });
number_for!(vector2_bool, |a| -> capi::c_int unsafe { nonzero(a, 2) });
number_for!(vector3_bool, |a| -> capi::c_int unsafe { nonzero(a, 3) });
number_for!(vector4_bool, |a| -> capi::c_int unsafe { nonzero(a, 4) });
number_for!(vector2_inplace_add, |a, b| unsafe {
    inplace_add_or_subtract(a, b, 2, true)
});
number_for!(vector2_inplace_subtract, |a, b| unsafe {
    inplace_add_or_subtract(a, b, 2, false)
});
number_for!(vector2_inplace_multiply, |a, b| unsafe {
    inplace_scalar(a, b, true)
});
number_for!(vector2_inplace_divide, |a, b| unsafe {
    inplace_scalar(a, b, false)
});
number_for!(vector3_inplace_add, |a, b| unsafe {
    inplace_add_or_subtract(a, b, 3, true)
});
number_for!(vector3_inplace_subtract, |a, b| unsafe {
    inplace_add_or_subtract(a, b, 3, false)
});
number_for!(vector3_inplace_multiply, |a, b| unsafe {
    inplace_multiply3(a, b)
});
number_for!(vector3_inplace_divide, |a, b| unsafe {
    inplace_scalar(a, b, false)
});
number_for!(vector4_inplace_add, |a, b| unsafe {
    inplace_add_or_subtract(a, b, 4, true)
});
number_for!(vector4_inplace_subtract, |a, b| unsafe {
    inplace_add_or_subtract(a, b, 4, false)
});
number_for!(vector4_inplace_multiply, |a, b| unsafe {
    inplace_scalar(a, b, true)
});
number_for!(vector4_inplace_divide, |a, b| unsafe {
    inplace_scalar(a, b, false)
});

/// `pyVector_richCompareSameWidth` (`script_math.cpp:2452-2486`): the
/// lexicographic `operator<` of the underlying C++ vector decides every
/// answer.
fn lexicographically_less(a: [f32; 4], b: [f32; 4], dim: usize) -> bool {
    for index in 0..dim {
        if a[index] != b[index] {
            return a[index] < b[index];
        }
    }
    false
}

/// `PyVector_tp_richcompare` (`script_math.cpp:2488-2501`): same-width
/// pairs compare; every other pair defers to the other operand.
///
/// # Safety
///
/// A C rich-compare call; the interpreter holds the GIL.
unsafe extern "C" fn rich_compare(
    v: *mut capi::PyObject,
    w: *mut capi::PyObject,
    op: capi::c_int,
) -> *mut capi::PyObject {
    for dim in 2..=4 {
        if unsafe { check_type(v, vector_name(dim)) } && unsafe { check_type(w, vector_name(dim)) }
        {
            // SAFETY: the checks above guarantee the instance shapes.
            let a = unsafe { values_of(v) };
            let b = unsafe { values_of(w) };
            let less = lexicographically_less(a, b, dim);
            let greater = lexicographically_less(b, a, dim);
            let answer = match op {
                capi::Py_EQ => !less && !greater,
                capi::Py_NE => less || greater,
                capi::Py_LT => less,
                capi::Py_GT => greater,
                capi::Py_LE => !greater,
                capi::Py_GE => !less,
                _ => return capi::py_not_implemented(),
            };
            return bool_object(answer);
        }
    }
    capi::py_not_implemented()
}

/// `PyVector<V>::pyStr` (`script_math.cpp:1361-1390`): `(x, y, z)` with the
/// C++ `ostream` default rendering per component.
///
/// # Safety
///
/// A C `tp_str` call; the interpreter holds the GIL.
unsafe fn py_str(self_: *mut capi::PyObject, dim: usize) -> *mut capi::PyObject {
    // SAFETY: the caller guarantees the instance shape.
    let values = unsafe { values_of(self_) };
    let components = (0..dim)
        .map(|index| cpp_float(values[index] as f64))
        .collect::<Vec<_>>()
        .join(", ");
    // SAFETY: the interpreter is up.
    unsafe { capi::unicode_from_bytes(format!("({components})").as_bytes()) }
}

/// # Safety
///
/// A C `tp_str` call; the interpreter holds the GIL.
unsafe extern "C" fn vector2_str(self_: *mut capi::PyObject) -> *mut capi::PyObject {
    // SAFETY: a C `tp_str` call.
    unsafe { py_str(self_, 2) }
}

/// # Safety
///
/// A C `tp_str` call; the interpreter holds the GIL.
unsafe extern "C" fn vector3_str(self_: *mut capi::PyObject) -> *mut capi::PyObject {
    // SAFETY: a C `tp_str` call.
    unsafe { py_str(self_, 3) }
}

/// # Safety
///
/// A C `tp_str` call; the interpreter holds the GIL.
unsafe extern "C" fn vector4_str(self_: *mut capi::PyObject) -> *mut capi::PyObject {
    // SAFETY: a C `tp_str` call.
    unsafe { py_str(self_, 4) }
}

/// The sequence handlers (`script_math.cpp:1683-1789`): the fixed length,
/// the bounds-checked indexed read and write. The binding's mapping bridge
/// normalises negative indices before either runs.
///
/// # Safety
///
/// A C sequence call; the interpreter holds the GIL.
unsafe extern "C" fn sq_length(self_: *mut capi::PyObject) -> isize {
    // SAFETY: the caller guarantees the instance shape.
    let dim = unsafe { dim_of(self_) }.unwrap_or(0);
    dim as isize
}

/// # Safety
///
/// A C sequence call; the interpreter holds the GIL.
unsafe extern "C" fn sq_item(self_: *mut capi::PyObject, index: isize) -> *mut capi::PyObject {
    // SAFETY: the caller guarantees the instance shape.
    let dim = unsafe { dim_of(self_) }.unwrap_or(0);
    if index < 0 || dim as isize <= index {
        raise_index_error("Vector index out of range");
        return std::ptr::null_mut();
    }
    // SAFETY: the caller guarantees the instance shape.
    let values = unsafe { values_of(self_) };
    component_object(values, index as usize)
}

/// # Safety
///
/// A C sequence call; the interpreter holds the GIL.
unsafe extern "C" fn sq_ass_item(
    self_: *mut capi::PyObject,
    index: isize,
    value: *mut capi::PyObject,
) -> capi::c_int {
    // SAFETY: the caller guarantees the instance shape.
    let dim = unsafe { dim_of(self_) }.unwrap_or(0);
    if index < 0 || dim as isize <= index {
        raise_index_error("Vector assignment index out of range");
        return -1;
    }
    // The component parse is `Script::setData(value, v[index], "Vector
    // element")` (`script_math.cpp:1774`).
    let Some(number) = (unsafe { capi::py_float_check(value) })
        .then(|| unsafe { capi::PyFloat_AsDouble(value) })
        .or_else(|| {
            (unsafe { capi::py_long_check(value) })
                .then(|| unsafe { capi::PyLong_AsLong(value) } as f64)
        })
    else {
        raise_type_error("Vector element must be set to a float");
        return -1;
    };
    // SAFETY: the caller guarantees the instance shape.
    let mut values = unsafe { values_of(self_) };
    values[index as usize] = number as f32;
    if !unsafe { safe_set_vector(self_, values) } {
        return -1;
    }
    0
}

/// One component getter over the index in its name (`x` is component 0 and
/// so on), the `pyGet_x` family (`script_math.cpp:2286-2303`).
macro_rules! component_getter {
    ($name:ident, $index:expr) => {
        /// # Safety
        ///
        /// A C getter call; the interpreter holds the GIL.
        unsafe extern "C" fn $name(
            object: *mut capi::PyObject,
            _closure: *mut std::os::raw::c_void,
        ) -> *mut capi::PyObject {
            // SAFETY: the caller guarantees the instance shape.
            let values = unsafe { values_of(object) };
            component_object(values, $index)
        }
    };
}

component_getter!(get_x, 0);
component_getter!(get_y, 1);
component_getter!(get_z, 2);
component_getter!(get_w, 3);

/// One component setter (`script_math.cpp:2305-2343`): parse into a copy,
/// write through `safeSetVector`.
macro_rules! component_setter {
    ($name:ident, $index:expr) => {
        /// # Safety
        ///
        /// A C setter call; the interpreter holds the GIL.
        unsafe extern "C" fn $name(
            object: *mut capi::PyObject,
            value: *mut capi::PyObject,
            _closure: *mut std::os::raw::c_void,
        ) -> capi::c_int {
            // The bare component parse is `Script::setData(value, v[i])`
            // with the default empty variable name, whose sentence carries
            // the leading space (`script.cpp:142`).
            let Some(number) = (unsafe { coerce_float(value, "") }) else {
                return -1;
            };
            // SAFETY: the caller guarantees the instance shape.
            let mut values = unsafe { values_of(object) };
            values[$index] = number as f32;
            if !unsafe { safe_set_vector(object, values) } {
                return -1;
            }
            0
        }
    };
}

component_setter!(set_x, 0);
component_setter!(set_y, 1);
component_setter!(set_z, 2);
component_setter!(set_w, 3);

/// `length`: the square root of the squared length of the live components
/// (`script_math.hpp:340`).
///
/// # Safety
///
/// A C getter call; the interpreter holds the GIL.
unsafe extern "C" fn get_length(
    object: *mut capi::PyObject,
    _closure: *mut std::os::raw::c_void,
) -> *mut capi::PyObject {
    // SAFETY: the caller guarantees the instance shape.
    let dim = unsafe { dim_of(object) }.unwrap_or(0);
    let values = unsafe { values_of(object) };
    let squared: f32 = (0..dim).map(|index| values[index] * values[index]).sum();
    float_object(squared.sqrt() as f64)
}

/// # Safety
///
/// A C getter call; the interpreter holds the GIL.
unsafe extern "C" fn get_length_squared(
    object: *mut capi::PyObject,
    _closure: *mut std::os::raw::c_void,
) -> *mut capi::PyObject {
    // SAFETY: the caller guarantees the instance shape.
    let dim = unsafe { dim_of(object) }.unwrap_or(0);
    let values = unsafe { values_of(object) };
    let squared: f32 = (0..dim).map(|index| values[index] * values[index]).sum();
    float_object(squared as f64)
}

/// `isReference` (`script_math.hpp:271`): a `PyVectorCopy` never wraps
/// foreign memory.
///
/// # Safety
///
/// A C getter call; the interpreter holds the GIL.
unsafe extern "C" fn get_is_reference(
    _object: *mut capi::PyObject,
    _closure: *mut std::os::raw::c_void,
) -> *mut capi::PyObject {
    bool_object(false)
}

/// # Safety
///
/// A C getter call; the interpreter holds the GIL.
unsafe extern "C" fn get_is_read_only(
    object: *mut capi::PyObject,
    _closure: *mut std::os::raw::c_void,
) -> *mut capi::PyObject {
    // SAFETY: the caller guarantees the instance shape.
    let read_only = unsafe { (*body::<VectorBody>(object)).read_only };
    bool_object(read_only)
}

/// The Vector3 yaw (`script_math.cpp:2225-2230` wrapping `vector3.ipp:548-551`).
///
/// # Safety
///
/// A C getter call; the interpreter holds the GIL.
unsafe extern "C" fn get_yaw(
    object: *mut capi::PyObject,
    _closure: *mut std::os::raw::c_void,
) -> *mut capi::PyObject {
    // SAFETY: the caller guarantees the instance shape.
    let values = unsafe { values_of(object) };
    float_object(values[0].atan2(values[2]) as f64)
}

/// The Vector3 pitch (`vector3.ipp:538-541`).
///
/// # Safety
///
/// A C getter call; the interpreter holds the GIL.
unsafe extern "C" fn get_pitch(
    object: *mut capi::PyObject,
    _closure: *mut std::os::raw::c_void,
) -> *mut capi::PyObject {
    // SAFETY: the caller guarantees the instance shape.
    let values = unsafe { values_of(object) };
    let flat = (values[0] * values[0] + values[2] * values[2]).sqrt();
    float_object(-values[1].atan2(flat) as f64)
}

/// The `set` method: the `py_set` parse and write
/// (`script_math.cpp:1798-1866`).
///
/// # Safety
///
/// A C method call; the interpreter holds the GIL.
unsafe extern "C" fn method_set(
    self_: *mut capi::PyObject,
    args: *mut capi::PyObject,
) -> *mut capi::PyObject {
    // SAFETY: the caller guarantees the instance shape.
    let dim = unsafe { dim_of(self_) }.unwrap_or(0);
    let Some(values) = (unsafe { parse_set_form(args, dim) }) else {
        raise_set_failed(dim);
        return std::ptr::null_mut();
    };
    if !unsafe { safe_set_vector(self_, values) } {
        return std::ptr::null_mut();
    }
    capi::py_none()
}

/// The `scale` method (`script_math.cpp:1868-1884`): a fresh scaled copy.
///
/// # Safety
///
/// A C method call; the interpreter holds the GIL.
unsafe extern "C" fn method_scale(
    self_: *mut capi::PyObject,
    args: *mut capi::PyObject,
) -> *mut capi::PyObject {
    // SAFETY: the caller guarantees the instance shape.
    let dim = unsafe { dim_of(self_) }.unwrap_or(0);
    let factor = (unsafe { capi::PyTuple_Size(args) } == 1)
        .then(|| unsafe { capi::PyTuple_GetItem(args, 0) })
        .and_then(|item| unsafe { coerce_float(item, "") });
    let Some(factor) = factor else {
        unsafe { capi::clear_error() };
        raise_type_error("Vector.scale expects a float argument");
        return std::ptr::null_mut();
    };
    // SAFETY: the caller guarantees the instance shape.
    let values = unsafe { values_of(self_) }.map(|component| component * factor as f32);
    own_raw(unsafe { vector_object(values, dim) })
}

/// The `dot` method (`script_math.cpp:1886-1908`): the argument goes
/// through the constructor forms, so `v.dot()` dots with the zero vector.
///
/// # Safety
///
/// A C method call; the interpreter holds the GIL.
unsafe extern "C" fn method_dot(
    self_: *mut capi::PyObject,
    args: *mut capi::PyObject,
) -> *mut capi::PyObject {
    // SAFETY: the caller guarantees the instance shape.
    let dim = unsafe { dim_of(self_) }.unwrap_or(0);
    // SAFETY: the interpreter is up and the argument tuple is live.
    let Some(other) = (unsafe { construct_from_args(args, dim) }) else {
        return std::ptr::null_mut();
    };
    // SAFETY: both are instances of vector types of this module.
    let a = unsafe { values_of(self_) };
    let b = unsafe { values_of(other.as_ptr()) };
    float_object((0..dim).map(|index| a[index] * b[index]).sum::<f32>() as f64)
}

/// The `normalise` method (`script_math.cpp:1910-1929`): in place, answers
/// `None`; a zero vector stays a zero vector.
///
/// # Safety
///
/// A C method call; the interpreter holds the GIL.
unsafe extern "C" fn method_normalise(
    self_: *mut capi::PyObject,
    args: *mut capi::PyObject,
) -> *mut capi::PyObject {
    if unsafe { capi::PyTuple_Size(args) } != 0 {
        raise_type_error("Vector.normalise takes no arguments (nor does it brook any dissent :)");
        return std::ptr::null_mut();
    }
    // SAFETY: the caller guarantees the instance shape.
    let dim = unsafe { dim_of(self_) }.unwrap_or(0);
    let values = unsafe { values_of(self_) };
    let squared: f32 = (0..dim).map(|index| values[index] * values[index]).sum();
    if squared > 0.0 {
        // The C++ `normalise` divides by the length only when it can.
        let length = squared.sqrt();
        let scaled: [f32; 4] = values.map(|component| component / length);
        if !unsafe { safe_set_vector(self_, scaled) } {
            return std::ptr::null_mut();
        }
    }
    capi::py_none()
}

/// The `tuple` method (`script_math.cpp:1931-1949`).
///
/// # Safety
///
/// A C method call; the interpreter holds the GIL.
unsafe extern "C" fn method_tuple(
    self_: *mut capi::PyObject,
    args: *mut capi::PyObject,
) -> *mut capi::PyObject {
    if unsafe { capi::PyTuple_Size(args) } != 0 {
        takes_no_arguments("tuple");
        // The raised `TypeError` must be answered with the null object; a
        // non-null return with an error pending is CPython's `SystemError`.
        return std::ptr::null_mut();
    }
    // SAFETY: the caller guarantees the instance shape.
    let dim = unsafe { dim_of(self_) }.unwrap_or(0);
    let values = unsafe { values_of(self_) };
    // SAFETY: the interpreter is up.
    let tuple = unsafe { capi::PyTuple_New(dim as isize) };
    for index in 0..dim {
        // SAFETY: the tuple is owned by this call and the index is in
        // bounds; `SetItem` adopts the component's reference.
        unsafe {
            capi::PyTuple_SetItem(tuple, index as isize, component_object(values, index));
        }
    }
    tuple
}

/// The `list` method (`script_math.cpp:1951-1965`).
///
/// # Safety
///
/// A C method call; the interpreter holds the GIL.
unsafe extern "C" fn method_list(
    self_: *mut capi::PyObject,
    args: *mut capi::PyObject,
) -> *mut capi::PyObject {
    if unsafe { capi::PyTuple_Size(args) } != 0 {
        takes_no_arguments("list");
        // As above: the pending error wants the null object back.
        return std::ptr::null_mut();
    }
    // SAFETY: the caller guarantees the instance shape.
    let dim = unsafe { dim_of(self_) }.unwrap_or(0);
    let values = unsafe { values_of(self_) };
    // SAFETY: the interpreter is up.
    let list = unsafe { capi::PyList_New(dim as isize) };
    for index in 0..dim {
        // SAFETY: as in `tuple`, the index is in bounds and `SetItem`
        // adopts the component's reference.
        unsafe {
            capi::PyList_SetItem(list, index as isize, component_object(values, index));
        }
    }
    list
}

/// The `cross2D` default (`script_math.cpp:1967-1973`): the dimensions
/// that do not specialise it refuse.
///
/// # Safety
///
/// A C method call; the interpreter holds the GIL.
unsafe extern "C" fn method_cross2d(
    _self_: *mut capi::PyObject,
    _args: *mut capi::PyObject,
) -> *mut capi::PyObject {
    raise_type_error("No cross2D for this type of vector");
    std::ptr::null_mut()
}

/// The Vector2 cross2D (`script_math.cpp:1975-1994`).
///
/// # Safety
///
/// A C method call; the interpreter holds the GIL.
unsafe extern "C" fn method_cross2d2(
    self_: *mut capi::PyObject,
    args: *mut capi::PyObject,
) -> *mut capi::PyObject {
    // SAFETY: the interpreter is up and the argument tuple is live.
    let Some(other) = (unsafe { construct_from_args(args, 2) }) else {
        return std::ptr::null_mut();
    };
    // SAFETY: both are vector instances.
    let a = unsafe { values_of(self_) };
    let b = unsafe { values_of(other.as_ptr()) };
    float_object((a[0] * b[1] - a[1] * b[0]) as f64)
}

/// The Vector3 cross2D (`script_math.cpp:1996-2014`): the XZ-plane cross.
///
/// # Safety
///
/// A C method call; the interpreter holds the GIL.
unsafe extern "C" fn method_cross2d3(
    self_: *mut capi::PyObject,
    args: *mut capi::PyObject,
) -> *mut capi::PyObject {
    // SAFETY: the interpreter is up and the argument tuple is live.
    let Some(other) = (unsafe { construct_from_args(args, 3) }) else {
        return std::ptr::null_mut();
    };
    // SAFETY: both are vector instances.
    let a = unsafe { values_of(self_) };
    let b = unsafe { values_of(other.as_ptr()) };
    float_object((a[0] * b[2] - a[2] * b[0]) as f64)
}

/// The `distTo` / `distSqrTo` / `flatDist*` parse (`script_math.cpp:2016-2060`):
/// one vector-like argument, or the whole argument tuple as a same-width
/// vector-like set.
///
/// # Safety
///
/// A C method call; the interpreter holds the GIL.
unsafe fn parse_dist_argument(args: *mut capi::PyObject, dim: usize) -> Option<[f32; 4]> {
    if unsafe { capi::PyTuple_Size(args) } == 1 {
        // SAFETY: the size check bounds the index.
        let item = unsafe { capi::PyTuple_GetItem(args, 0) };
        if let Some(values) = unsafe { parse_vector_like(item, dim, "argument 1") } {
            return Some(values);
        }
    }
    unsafe { parse_vector_like(args, dim, "argument 1") }
}

/// # Safety
///
/// A C method call; the interpreter holds the GIL.
unsafe extern "C" fn method_dist_to(
    self_: *mut capi::PyObject,
    args: *mut capi::PyObject,
) -> *mut capi::PyObject {
    // SAFETY: the caller guarantees the instance shape.
    let dim = unsafe { dim_of(self_) }.unwrap_or(0);
    let Some(other) = (unsafe { parse_dist_argument(args, dim) }) else {
        return std::ptr::null_mut();
    };
    // SAFETY: the caller guarantees the instance shape.
    let values = unsafe { values_of(self_) };
    let squared: f32 = (0..dim)
        .map(|index| (values[index] - other[index]).powi(2))
        .sum();
    float_object(squared.sqrt() as f64)
}

/// # Safety
///
/// A C method call; the interpreter holds the GIL.
unsafe extern "C" fn method_dist_sqr_to(
    self_: *mut capi::PyObject,
    args: *mut capi::PyObject,
) -> *mut capi::PyObject {
    // SAFETY: the caller guarantees the instance shape.
    let dim = unsafe { dim_of(self_) }.unwrap_or(0);
    let Some(other) = (unsafe { parse_dist_argument(args, dim) }) else {
        return std::ptr::null_mut();
    };
    // SAFETY: the caller guarantees the instance shape.
    let values = unsafe { values_of(self_) };
    let squared: f32 = (0..dim)
        .map(|index| (values[index] - other[index]).powi(2))
        .sum();
    float_object(squared as f64)
}

/// # Safety
///
/// A C method call; the interpreter holds the GIL.
unsafe extern "C" fn method_flat_dist_to(
    _self_: *mut capi::PyObject,
    _args: *mut capi::PyObject,
) -> *mut capi::PyObject {
    raise_type_error("No flatDistTo for this type of vector");
    std::ptr::null_mut()
}

/// # Safety
///
/// A C method call; the interpreter holds the GIL.
unsafe extern "C" fn method_flat_dist_sqr_to(
    _self_: *mut capi::PyObject,
    _args: *mut capi::PyObject,
) -> *mut capi::PyObject {
    raise_type_error("No flatDistSqrTo for this type of vector");
    std::ptr::null_mut()
}

/// The Vector3 flatDistTo (`script_math.cpp:2110-2132`): the XZ-plane
/// distance.
///
/// # Safety
///
/// A C method call; the interpreter holds the GIL.
unsafe extern "C" fn method_flat_dist_to3(
    self_: *mut capi::PyObject,
    args: *mut capi::PyObject,
) -> *mut capi::PyObject {
    let Some(other) = (unsafe { parse_dist_argument(args, 3) }) else {
        return std::ptr::null_mut();
    };
    // SAFETY: the caller guarantees the instance shape.
    let values = unsafe { values_of(self_) };
    let x = values[0] - other[0];
    let z = values[2] - other[2];
    float_object((x * x + z * z).sqrt() as f64)
}

/// The Vector3 flatDistSqrTo (`script_math.cpp:2134-2157`).
///
/// # Safety
///
/// A C method call; the interpreter holds the GIL.
unsafe extern "C" fn method_flat_dist_sqr_to3(
    self_: *mut capi::PyObject,
    args: *mut capi::PyObject,
) -> *mut capi::PyObject {
    let Some(other) = (unsafe { parse_dist_argument(args, 3) }) else {
        return std::ptr::null_mut();
    };
    // SAFETY: the caller guarantees the instance shape.
    let values = unsafe { values_of(self_) };
    let x = values[0] - other[0];
    let z = values[2] - other[2];
    float_object((x * x + z * z) as f64)
}

/// # Safety
///
/// A C method call; the interpreter holds the GIL.
unsafe extern "C" fn method_set_pitch_yaw(
    _self_: *mut capi::PyObject,
    _args: *mut capi::PyObject,
) -> *mut capi::PyObject {
    raise_type_error("No setPitchYaw for this type of vector");
    std::ptr::null_mut()
}

/// The Vector3 setPitchYaw (`script_math.cpp:2159-2194`): exactly two
/// angles, in place.
///
/// # Safety
///
/// A C method call; the interpreter holds the GIL.
unsafe extern "C" fn method_set_pitch_yaw3(
    self_: *mut capi::PyObject,
    args: *mut capi::PyObject,
) -> *mut capi::PyObject {
    if unsafe { capi::PyTuple_Size(args) } != 2 {
        raise_type_error("Vector3.setPitchYaw expects exactly two arguments");
        return std::ptr::null_mut();
    }
    // SAFETY: the size check above bounds both indices.
    let (first, second) = (unsafe { capi::PyTuple_GetItem(args, 0) }, unsafe {
        capi::PyTuple_GetItem(args, 1)
    });
    let (Some(pitch), Some(yaw)) = (unsafe { coerce_float(first, "") }, unsafe {
        coerce_float(second, "")
    }) else {
        return std::ptr::null_mut();
    };
    // `Vector3::setPitchYaw` (`vector3.ipp:520-534`) computes in doubles
    // and stores the float truncations.
    let cos_pitch = pitch.cos();
    let sin_pitch = (-pitch).sin();
    let cos_yaw = yaw.cos();
    let sin_yaw = yaw.sin();
    let values = [
        (cos_pitch * sin_yaw) as f32,
        sin_pitch as f32,
        (cos_pitch * cos_yaw) as f32,
        0.0,
    ];
    if !unsafe { safe_set_vector(self_, values) } {
        return std::ptr::null_mut();
    }
    capi::py_none()
}

/// `__getstate__` (`script_math.cpp:2196-2205`): the live components as a
/// little-endian float blob.
///
/// # Safety
///
/// A C method call; the interpreter holds the GIL.
unsafe extern "C" fn method_get_state(
    self_: *mut capi::PyObject,
    _args: *mut capi::PyObject,
) -> *mut capi::PyObject {
    // SAFETY: the caller guarantees the instance shape.
    let dim = unsafe { dim_of(self_) }.unwrap_or(0);
    let values = unsafe { values_of(self_) };
    let mut blob = [0u8; 16];
    for (index, component) in values.iter().take(dim).enumerate() {
        blob[index * 4..index * 4 + 4].copy_from_slice(&component.to_le_bytes());
    }
    // SAFETY: the interpreter is up.
    unsafe { capi::PyBytes_FromStringAndSize(blob.as_ptr().cast(), dim as isize * 4) }
}

/// `__setstate__` (`script_math.cpp:2207-2232`): a one-tuple of a bytes
/// blob of the exact instance width.
///
/// # Safety
///
/// A C method call; the interpreter holds the GIL.
unsafe extern "C" fn method_set_state(
    self_: *mut capi::PyObject,
    args: *mut capi::PyObject,
) -> *mut capi::PyObject {
    // SAFETY: the caller guarantees the instance shape.
    let dim = unsafe { dim_of(self_) }.unwrap_or(0);
    let accepted =
        unsafe { capi::py_tuple_check(args) } && unsafe { capi::PyTuple_Size(args) } == 1;
    if accepted {
        // SAFETY: the size check bounds the index.
        let item = unsafe { capi::PyTuple_GetItem(args, 0) };
        if unsafe { capi::py_bytes_check(item) } {
            let mut pointer: *mut std::os::raw::c_char = std::ptr::null_mut();
            let mut length: isize = 0;
            // SAFETY: the check above guarantees a bytes object.
            if unsafe { capi::PyBytes_AsStringAndSize(item, &mut pointer, &mut length) } == 0
                && length == dim as isize * 4
            {
                let mut values = [0.0_f32; 4];
                let bytes =
                    unsafe { std::slice::from_raw_parts(pointer.cast::<u8>(), length as usize) };
                for (index, component) in values.iter_mut().take(dim).enumerate() {
                    let start = index * 4;
                    *component = f32::from_le_bytes([
                        bytes[start],
                        bytes[start + 1],
                        bytes[start + 2],
                        bytes[start + 3],
                    ]);
                }
                if !unsafe { safe_set_vector(self_, values) } {
                    return std::ptr::null_mut();
                }
                return capi::py_none();
            }
        }
    }
    raise_type_error("__setstate__ called with invalid type\n");
    std::ptr::null_mut()
}

/// The one method table all three dimensions carry
/// (`script_math.cpp:2498-2583`); the per-dimension refusals happen at
/// runtime.
fn with_methods(spec: TypeSpec, dim: usize) -> TypeSpec {
    spec.method("set", method_set)
        .method("scale", method_scale)
        .method("dot", method_dot)
        .method("normalise", method_normalise)
        .method("tuple", method_tuple)
        .method("list", method_list)
        // The per-dimension specializations (`script_math.cpp:1985`,
        // `:2004`, `:2078`, `:2152`); the other dimensions answer the
        // template's refusal.
        .method(
            "cross2D",
            match dim {
                2 => method_cross2d2,
                3 => method_cross2d3,
                _ => method_cross2d,
            },
        )
        .method("distTo", method_dist_to)
        .method("distSqrTo", method_dist_sqr_to)
        .method(
            "flatDistTo",
            match dim {
                3 => method_flat_dist_to3,
                _ => method_flat_dist_to,
            },
        )
        .method(
            "flatDistSqrTo",
            match dim {
                3 => method_flat_dist_sqr_to3,
                _ => method_flat_dist_sqr_to,
            },
        )
        .method(
            "setPitchYaw",
            match dim {
                3 => method_set_pitch_yaw3,
                _ => method_set_pitch_yaw,
            },
        )
        .method("__getstate__", method_get_state)
        .method("__setstate__", method_set_state)
}

/// Build one dimension's full type description
/// (`script_math.cpp:2583-2619` and the `DEFINE_PYVECTOR_TYPEOBJECT` slot
/// wiring): the attributes per dimension, the shared sequence handlers and
/// the per-dimension number slots.
fn vector_spec(dim: usize) -> TypeSpec {
    let base = TypeSpec::new(vector_name(dim))
        .instance_size(std::mem::size_of::<VectorBody>())
        .attribute("isReference", get_is_reference, None)
        .attribute("isReadOnly", get_is_read_only, None)
        .attribute("length", get_length, None)
        .attribute("lengthSquared", get_length_squared, None)
        .attribute("x", get_x, Some(set_x))
        .attribute("y", get_y, Some(set_y));
    let base = if dim >= 3 {
        base.attribute("z", get_z, Some(set_z))
    } else {
        base
    };
    let base = if dim == 3 {
        base.attribute("yaw", get_yaw, None)
            .attribute("pitch", get_pitch, None)
    } else {
        base
    };
    let base = if dim >= 4 {
        base.attribute("w", get_w, Some(set_w))
    } else {
        base
    };
    let base = with_methods(base, dim).sequence(sq_length, sq_item, sq_ass_item);
    base.slot(
        capi::Py_tp_new,
        match dim {
            2 => vector2_new as *mut std::os::raw::c_void,
            3 => vector3_new as *mut std::os::raw::c_void,
            _ => vector4_new as *mut std::os::raw::c_void,
        },
    )
    .slot(
        capi::Py_tp_str,
        match dim {
            2 => vector2_str as *mut std::os::raw::c_void,
            3 => vector3_str as *mut std::os::raw::c_void,
            _ => vector4_str as *mut std::os::raw::c_void,
        },
    )
    .slot(
        // `repr(v) == str(v) == "(x, y, z)"`: the reference wires both slots
        // to the same `PyVector<V>::_pyStr`
        // (`PY_TYPEOBJECT_SPECIALISE_REPR_AND_STR`, script_math.cpp:2520).
        capi::Py_tp_repr,
        match dim {
            2 => vector2_str as *mut std::os::raw::c_void,
            3 => vector3_str as *mut std::os::raw::c_void,
            _ => vector4_str as *mut std::os::raw::c_void,
        },
    )
    .slot(
        capi::Py_tp_richcompare,
        rich_compare as *mut std::os::raw::c_void,
    )
    .slot(
        capi::Py_nb_add,
        match dim {
            2 => vector2_add as *mut std::os::raw::c_void,
            3 => vector3_add as *mut std::os::raw::c_void,
            _ => vector4_add as *mut std::os::raw::c_void,
        },
    )
    .slot(
        capi::Py_nb_subtract,
        match dim {
            2 => vector2_subtract as *mut std::os::raw::c_void,
            3 => vector3_subtract as *mut std::os::raw::c_void,
            _ => vector4_subtract as *mut std::os::raw::c_void,
        },
    )
    .slot(
        capi::Py_nb_multiply,
        match dim {
            2 => vector2_multiply as *mut std::os::raw::c_void,
            3 => vector3_multiply as *mut std::os::raw::c_void,
            _ => vector4_multiply as *mut std::os::raw::c_void,
        },
    )
    .slot(
        capi::Py_nb_true_divide,
        match dim {
            2 => vector2_divide as *mut std::os::raw::c_void,
            3 => vector3_divide as *mut std::os::raw::c_void,
            _ => vector4_divide as *mut std::os::raw::c_void,
        },
    )
    .slot(
        capi::Py_nb_negative,
        match dim {
            2 => vector2_negative as *mut std::os::raw::c_void,
            3 => vector3_negative as *mut std::os::raw::c_void,
            _ => vector4_negative as *mut std::os::raw::c_void,
        },
    )
    .slot(
        capi::Py_nb_positive,
        match dim {
            2 => vector2_positive as *mut std::os::raw::c_void,
            3 => vector3_positive as *mut std::os::raw::c_void,
            _ => vector4_positive as *mut std::os::raw::c_void,
        },
    )
    .slot(
        capi::Py_nb_bool,
        match dim {
            2 => vector2_bool as *mut std::os::raw::c_void,
            3 => vector3_bool as *mut std::os::raw::c_void,
            _ => vector4_bool as *mut std::os::raw::c_void,
        },
    )
    .slot(
        capi::Py_nb_inplace_add,
        match dim {
            2 => vector2_inplace_add as *mut std::os::raw::c_void,
            3 => vector3_inplace_add as *mut std::os::raw::c_void,
            _ => vector4_inplace_add as *mut std::os::raw::c_void,
        },
    )
    .slot(
        capi::Py_nb_inplace_subtract,
        match dim {
            2 => vector2_inplace_subtract as *mut std::os::raw::c_void,
            3 => vector3_inplace_subtract as *mut std::os::raw::c_void,
            _ => vector4_inplace_subtract as *mut std::os::raw::c_void,
        },
    )
    .slot(
        capi::Py_nb_inplace_multiply,
        match dim {
            2 => vector2_inplace_multiply as *mut std::os::raw::c_void,
            3 => vector3_inplace_multiply as *mut std::os::raw::c_void,
            _ => vector4_inplace_multiply as *mut std::os::raw::c_void,
        },
    )
    .slot(
        capi::Py_nb_inplace_true_divide,
        match dim {
            2 => vector2_inplace_divide as *mut std::os::raw::c_void,
            3 => vector3_inplace_divide as *mut std::os::raw::c_void,
            _ => vector4_inplace_divide as *mut std::os::raw::c_void,
        },
    )
}

/// Attach one created type to the `Math` module dictionary under its bare
/// name -- the `PyFactoryMethodLink` step (`py_factory_method_link.cpp:37-80`).
///
/// # Safety
///
/// The interpreter must be up; `module` must be the `Math` module object.
unsafe fn attach(module: *mut capi::PyObject, name: std::ffi::CString, object: &PyRef) {
    // SAFETY: the module is a live module object and the name is a
    // NUL-terminated key.
    unsafe {
        capi::PyDict_SetItemString(
            capi::PyModule_GetDict(module),
            name.as_ptr(),
            object.as_ptr(),
        );
    }
}

/// The `Math` module installation step for the vectors: three types, the
/// `TempPyVectorInit` half of the reference rung-0 jobs
/// (`script_math.cpp:3053-3067`).
///
/// # Safety
///
/// The interpreter must be up on the calling thread.
pub(super) unsafe fn install(module: *mut capi::PyObject) {
    for dim in 2..=4 {
        let spec = vector_spec(dim);
        // SAFETY: the interpreter is up.
        let object = match binding::create_type(&spec) {
            Ok(object) => object,
            Err(_) => panic!("Math: the {} type could not be created", vector_name(dim)),
        };
        // The handlers recover their dimension from the type's address.
        dims()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(object.as_ptr() as usize, dim);
        remember_type(vector_name(dim), &object);
        // SAFETY: the module is live.
        unsafe { attach(module, bare_name(dim), &object) };
        // The type object the registry of `binding` holds keeps everything
        // alive; this handle can go.
        drop(object);
    }
}

/// The module-dictionary key of one dimension: the type name without the
/// `Math.` prefix.
fn bare_name(dim: usize) -> std::ffi::CString {
    std::ffi::CString::new(vector_name(dim).strip_prefix("Math.").expect("the prefix"))
        .expect("no embedded NUL")
}
