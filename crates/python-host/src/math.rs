//! The `Math` module: the `PyVector<Vector234>` value types
//! (`lib/pyscript/script_math.cpp`).
//!
//! The reference's `TempPyVectorInit` (`script_math.cpp:3053-3067`) builds
//! the `Math` module with `PyImport_AddModule` and `PyModule_AddObject`s one
//! type object per width; the port registers the same module the same way
//! from [`Interpreter::init`](crate::interpreter::Interpreter::init).
//!
//! One template covers all three widths in the reference; one generic core
//! plus one macro instantiation per width covers them here. The number
//! protocol is `PyVector_tp_as_number` (`:2338-2396`): `nb_add`/`nb_subtract`
//! take two same-width vectors (or tuples of that many floats, what
//! `Script::setData(Vector3)` accepts, `script.cpp:1905`), `nb_multiply` is
//! scalar scaling on every width and cross product for two `Vector3`s
//! (`:1487`), `nb_true_divide` is scalar-only, the inplace partners mutate
//! and return `self`, and `nb_bool` is `lengthSquared > 0`. Anything else
//! returns `Py_NotImplemented`, so the interpreter raises `TypeError` -- the
//! reference behaviour for `Vector3 + 5`, `2 / v` and friends.
//!
//! Evaluation order is the acceptance: the port keeps the reference's
//! left-then-right operand checks and its exact accumulation orders (the
//! element-wise f32 arithmetic and the f32 `lengthSquared`), so a script
//! sees the same results and the same failures in the same order.

use std::ffi::c_char;
use std::ffi::c_int;
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::binding;
use crate::capi;
use crate::pyref::PyRef;

/// The instance body `binding` allocates for a vector: the `PyObject` header
/// the interpreter owns, then the components inline, the layout of the
/// reference's `PyVectorCopy<V>` (whose `V` sits right after the header).
#[repr(C)]
struct VectorBody<const N: usize> {
    header: capi::PyObject,
    data: [f32; N],
}

/// The instance body beyond the `PyObject` header: `N` inline f32s.
fn instance_size<const N: usize>() -> usize {
    std::mem::size_of::<VectorBody<N>>() - std::mem::size_of::<capi::PyObject>()
}

/// Read one of this module's vector instances. `None` for anything else.
///
/// # Safety
///
/// `object` must be a borrowed object the interpreter handed out, or null.
unsafe fn data_read<const N: usize>(
    object: *mut capi::PyObject,
    type_addr: usize,
) -> Option<[f32; N]> {
    if object.is_null() {
        return None;
    }
    // SAFETY: the caller holds a borrowed object; the type check gates the
    // body read to instances whose body really is a `VectorBody<N>`.
    unsafe {
        if capi::Py_TYPE(object) as usize != type_addr {
            return None;
        }
        Some(std::ptr::addr_of!((*object.cast::<VectorBody<N>>()).data).read())
    }
}

/// Write one of this module's vector instances; `false` for anything else.
///
/// # Safety
///
/// `object` must be a borrowed object the interpreter handed out.
unsafe fn data_write<const N: usize>(
    object: *mut capi::PyObject,
    type_addr: usize,
    data: [f32; N],
) -> bool {
    if object.is_null() {
        return false;
    }
    // SAFETY: as `data_read`; the write goes through the same checked body.
    unsafe {
        if capi::Py_TYPE(object) as usize != type_addr {
            return false;
        }
        std::ptr::addr_of_mut!((*object.cast::<VectorBody<N>>()).data).write(data);
    }
    true
}

/// One float out of a script object, `Script::setData(obj, float)`: an int
/// or a float converts, anything else fails without leaving an error (the
/// caller decides whether that is `NotImplemented` or a `TypeError`).
///
/// # Safety
///
/// `object` must be a borrowed object the interpreter handed out, or null.
unsafe fn as_float(object: *mut capi::PyObject) -> Option<f32> {
    if object.is_null() {
        return None;
    }
    // SAFETY: the conversion reads the borrowed object; a failure leaves a
    // pending error which is cleared here, the reference's `PyErr_Clear`
    // before its `Py_NotImplemented` arms.
    unsafe {
        let value = capi::PyFloat_AsDouble(object);
        if !capi::PyErr_Occurred().is_null() {
            capi::PyErr_Clear();
            return None;
        }
        Some(value as f32)
    }
}

/// `Script::setData(obj, float&)` (`script.cpp:1840-1877`): a float or an
/// int converts, anything else is refused. Unlike [`as_float`] this does not
/// accept an object with only a `__float__`, the reference's two accepted
/// types; an overflowing int is refused too, the reference ending in the
/// same `TypeError`.
///
/// # Safety
///
/// `object` must be a borrowed object the interpreter handed out, or null.
unsafe fn as_script_float(object: *mut capi::PyObject) -> Option<f32> {
    if object.is_null() {
        return None;
    }
    // SAFETY: the conversion reads the borrowed object; a failed `PyLong`
    // read leaves an error the caller replaces with its own.
    unsafe {
        if capi::py_float_check(object) {
            return Some(capi::PyFloat_AsDouble(object) as f32);
        }
        if capi::py_long_check(object) {
            let value = capi::PyLong_AsLong(object);
            if !capi::PyErr_Occurred().is_null() {
                capi::PyErr_Clear();
                return None;
            }
            return Some(value as f32);
        }
    }
    None
}

/// One vector operand out of a script object, `Script::setData(obj, Vector)`
/// (`script.cpp:1905`): a same-width `PyVector` or a tuple of that many
/// floats. A single float and a list are both refused, exactly what the
/// reference's `PyArg_ParseTuple`-based conversion does.
///
/// # Safety
///
/// `object` must be a borrowed object the interpreter handed out, or null.
unsafe fn as_vector<const N: usize>(
    object: *mut capi::PyObject,
    type_addr: usize,
) -> Option<[f32; N]> {
    if let Some(data) = unsafe { data_read::<N>(object, type_addr) } {
        return Some(data);
    }
    // SAFETY: the checks read the borrowed object; a failed component
    // conversion is cleared so the caller sees a clean refusal.
    unsafe {
        if object.is_null() || !capi::py_tuple_check(object) {
            return None;
        }
        if capi::PyTuple_Size(object) != N as capi::PySsize {
            return None;
        }
        let mut data = [0.0f32; N];
        for (index, slot) in data.iter_mut().enumerate() {
            // SAFETY: the size was checked against N above.
            let item = capi::PyTuple_GetItem(object, index as capi::PySsize);
            *slot = as_float(item)?;
        }
        Some(data)
    }
}

/// A tuple of exactly `N` floats, or `None` (any error is left clear).
///
/// # Safety
///
/// `tuple` must be a borrowed object the interpreter handed out, or null.
unsafe fn parse_sequence<const N: usize>(tuple: *mut capi::PyObject) -> Option<[f32; N]> {
    // SAFETY: the checks read the borrowed object.
    unsafe {
        if tuple.is_null() || !capi::py_tuple_check(tuple) {
            return None;
        }
        if capi::PyTuple_Size(tuple) != N as capi::PySsize {
            return None;
        }
        let mut data = [0.0f32; N];
        for (index, slot) in data.iter_mut().enumerate() {
            // SAFETY: the size was checked against N above.
            let item = capi::PyTuple_GetItem(tuple, index as capi::PySsize);
            *slot = as_float(item)?;
        }
        Some(data)
    }
}

/// `Script::setData(obj, VectorN)` as `py_distTo` and the other distance
/// methods call it (`script_math.cpp:2037-2137`): one argument that is a
/// same-width vector or a tuple of `N` floats, otherwise the whole tuple of
/// `N` floats. Unlike [`parse_into`], a single float is not a splat and a
/// list is not accepted -- both fail, as `setData` fails, leaving the
/// reference's own `TypeError` set (`script.cpp:1920-1921`).
///
/// # Safety
///
/// `args` must be the borrowed arguments tuple CPython handed over.
unsafe fn parse_operand<const N: usize>(
    args: *mut capi::PyObject,
    type_addr: usize,
) -> Option<[f32; N]> {
    // SAFETY: the reads are on borrowed objects from the call above.
    let parsed = unsafe {
        if capi::PyTuple_Size(args) == 1 {
            let item = capi::PyTuple_GetItem(args, 0);
            as_vector::<N>(item, type_addr)
        } else {
            parse_sequence::<N>(args)
        }
    };
    if parsed.is_none() {
        // SAFETY: the interpreter is up; the message is the width's static.
        unsafe {
            let message = match N {
                2 => c"argument 1 must be set to a Vector2 or a tuple of 2 floats".as_ptr(),
                3 => c"argument 1 must be set to a Vector3 or a tuple of 3 floats".as_ptr(),
                _ => c"argument 1 must be set to a Vector4 or a tuple of 4 floats".as_ptr(),
            };
            capi::PyErr_SetString(capi::PyExc_TypeError, message);
        }
    }
    parsed
}

/// `PyVector::py_set` (`script_math.cpp:1801-1856`): the argument table of
/// `set`, of the constructor and of every method that takes a vector. One
/// argument unwraps (a same-width vector, a single float splat, a tuple, a
/// list turned into a tuple); any other shape falls back to the argument
/// tuple itself read as `N` floats. A failure raises the reference's
/// `TypeError` and returns `None`.
///
/// # Safety
///
/// `args` must be a borrowed argument tuple the interpreter handed out.
unsafe fn parse_into<const N: usize>(
    args: *mut capi::PyObject,
    type_addr: usize,
) -> Option<[f32; N]> {
    let fail = || -> Option<[f32; N]> {
        // The reference's `PyErr_Format` text (`script_math.cpp:1851-1852`);
        // the message is built here because the port has no varargs FFI.
        // SAFETY: raising only sets the error; the interpreter is up
        // because the handler that got here is a C API callback.
        unsafe {
            let rendered =
                format!("Vector.set must be set to a sequence of {N} floats, or one float");
            let message =
                std::ffi::CString::new(rendered).expect("a formatted width has no embedded NUL");
            capi::PyErr_SetString(capi::PyExc_TypeError, message.as_ptr());
        }
        None
    };
    // SAFETY: every branch reads borrowed objects the interpreter handed
    // out; the list branch owns the converted tuple for its duration.
    unsafe {
        if capi::PyTuple_Size(args) == 1 {
            let item = capi::PyTuple_GetItem(args, 0);
            if capi::py_tuple_check(item) {
                return parse_sequence::<N>(item).or_else(fail);
            }
            if capi::py_list_check(item) {
                let tuple = capi::PyList_AsTuple(item);
                if tuple.is_null() {
                    return fail();
                }
                let parsed = parse_sequence::<N>(tuple);
                capi::dec_ref(tuple);
                return parsed.or_else(fail);
            }
            if let Some(data) = data_read::<N>(item, type_addr) {
                return Some(data);
            }
            if let Some(value) = as_float(item) {
                return Some([value; N]);
            }
            // A one-element shape none of the arms took: the reference
            // falls through to the whole tuple, which then also fails.
            return parse_sequence::<N>(args).or_else(fail);
        }
        parse_sequence::<N>(args).or_else(fail)
    }
}

/// `Py_INCREF(Py_NotImplemented)` and return it, the reference's deferral.
fn not_implemented() -> *mut capi::PyObject {
    // SAFETY: the singleton address is valid whenever the interpreter is up,
    // which every protocol handler's caller guarantees.
    unsafe {
        let sentinel = capi::not_implemented();
        capi::inc_ref(sentinel);
        sentinel
    }
}

/// `Py_RETURN_NONE`.
fn none_ref() -> *mut capi::PyObject {
    // SAFETY: as `not_implemented`.
    unsafe {
        let none = capi::py_none();
        capi::inc_ref(none);
        none
    }
}

/// One `Math.Vector3` argument, `Script::setData(obj, Vector3)`: the
/// same-width vector or a tuple of three floats.
fn parse_vec3(item: *mut capi::PyObject) -> Option<foundation::math::Vector3> {
    let data = unsafe { as_vector::<3>(item, vector3::type_addr()) };
    data.map(|d| foundation::math::Vector3::new(d[0], d[1], d[2]))
}

/// One `Math.Vector4` argument, `Script::setData(obj, Vector4)`.
fn parse_vec4(item: *mut capi::PyObject) -> Option<foundation::math::Vector4> {
    let data = unsafe { as_vector::<4>(item, vector4::type_addr()) };
    data.map(|d| foundation::math::Vector4::new(d[0], d[1], d[2], d[3]))
}

/// One unsigned integer argument, the reference's `uint` parameters.
fn parse_uint(item: *mut capi::PyObject) -> Option<usize> {
    if item.is_null() {
        return None;
    }
    // SAFETY: the conversion reads the borrowed object; a failure leaves
    // a pending error which is cleared here.
    unsafe {
        let value = capi::PyLong_AsLong(item);
        if !capi::PyErr_Occurred().is_null() {
            capi::PyErr_Clear();
            return None;
        }
        Some(value as usize)
    }
}

/// A float angle in the reference's `[-100, 100]` range
/// (`script_math.hpp:68-74`); `Err` carries the raised `TypeError`.
fn parse_angle(item: *mut capi::PyObject) -> Result<f32, *mut capi::PyObject> {
    let Some(value) = (unsafe { as_float(item) }) else {
        return Err(type_error("must be real number"));
    };
    if !(-100.0..=100.0).contains(&value) {
        return Err(type_error("must be a valid angle"));
    }
    Ok(value)
}

fn type_error(message: &str) -> *mut capi::PyObject {
    // SAFETY: raising only sets the error; the interpreter is up because
    // the handler that got here is a C API callback.
    unsafe {
        let message = std::ffi::CString::new(message).expect("a message has no embedded NUL");
        capi::PyErr_SetString(capi::PyExc_TypeError, message.as_ptr());
    }
    std::ptr::null_mut()
}

fn value_error(message: &str) -> *mut capi::PyObject {
    // SAFETY: as `type_error`.
    unsafe {
        let message = std::ffi::CString::new(message).expect("a message has no embedded NUL");
        capi::PyErr_SetString(capi::PyExc_ValueError, message.as_ptr());
    }
    std::ptr::null_mut()
}

/// The number of arguments the reference `PyArg_ParseTuple` would accept
/// for a handler's format; an unexpected count raises the same
/// `TypeError` family the format parser raises.
fn want_args(args: *mut capi::PyObject, count: usize) -> bool {
    // SAFETY: the count read is on the borrowed arguments tuple.
    unsafe { capi::PyTuple_Size(args) == count as capi::PySsize }
}

fn arg(args: *mut capi::PyObject, index: usize) -> *mut capi::PyObject {
    // SAFETY: the caller has already checked the tuple length.
    unsafe { capi::PyTuple_GetItem(args, index as capi::PySsize) }
}

/// `PyBool_FromLong`.
fn bool_ref(flag: bool) -> *mut capi::PyObject {
    // SAFETY: as `not_implemented`.
    unsafe {
        let value = if flag {
            capi::py_true()
        } else {
            capi::py_false()
        };
        capi::inc_ref(value);
        value
    }
}

/// The attribute name a getset handler serves, carried in the reference's
/// `PyGetSetDef.closure`; the `%s` of its error text. Shared by the
/// `Vector4Provider` and `MatrixProvider` slot families.
fn attribute_name(closure: *mut std::os::raw::c_void) -> String {
    if closure.is_null() {
        return "value".to_owned();
    }
    // SAFETY: the closure is the C string that lives for the type's
    // lifetime; an attribute name is ASCII.
    unsafe {
        std::ffi::CStr::from_ptr(closure.cast::<c_char>())
            .to_str()
            .unwrap_or("value")
            .to_owned()
    }
}

// --- The arithmetic core: element-wise f32, the reference's accumulation
// --- order (statement order, no widening).

fn vec_add<const N: usize>(a: [f32; N], b: [f32; N]) -> [f32; N] {
    let mut out = a;
    for (slot, other) in out.iter_mut().zip(b) {
        *slot += other;
    }
    out
}

fn vec_sub<const N: usize>(a: [f32; N], b: [f32; N]) -> [f32; N] {
    let mut out = a;
    for (slot, other) in out.iter_mut().zip(b) {
        *slot -= other;
    }
    out
}

fn vec_scale<const N: usize>(a: [f32; N], factor: f32) -> [f32; N] {
    let mut out = a;
    for slot in &mut out {
        *slot *= factor;
    }
    out
}

fn vec_dot<const N: usize>(a: [f32; N], b: [f32; N]) -> f32 {
    let mut total = 0.0f32;
    for (left, right) in a.iter().zip(b) {
        total += left * right;
    }
    total
}

fn vec_length_squared<const N: usize>(a: [f32; N]) -> f32 {
    vec_dot::<N>(a, a)
}

/// The reference `Vector::length()` family: f32 accumulation, f32 root.
fn vec_length<const N: usize>(a: [f32; N]) -> f32 {
    vec_length_squared::<N>(a).sqrt()
}

/// The reference `Vector::distance()`: widen to double, root, narrow back
/// (`vector3.ipp:250-254` -- the one place the reference widens).
fn vec_distance<const N: usize>(a: [f32; N], b: [f32; N]) -> f32 {
    let mut total = 0.0f64;
    for (left, right) in a.iter().zip(b) {
        let delta = (*left - right) as f64;
        total += delta * delta;
    }
    total.sqrt() as f32
}

/// `distSqrTo` (`script_math.cpp:2053`): `(this - v).lengthSquared()`, the
/// same f32 accumulation as `lengthSquared` itself.
fn vec_distance_squared<const N: usize>(a: [f32; N], b: [f32; N]) -> f32 {
    let mut total = 0.0f32;
    for (left, right) in a.iter().zip(b) {
        let delta = *left - right;
        total += delta * delta;
    }
    total
}

/// The reference `Vector::normalise()`: nothing within `almostZero(1e-8)`,
/// otherwise scale by `1/length`; the return is the pre-normalise length.
fn vec_normalise<const N: usize>(data: &mut [f32; N]) -> f32 {
    let length = vec_length::<N>(*data);
    if !(-1e-8..1e-8).contains(&length) {
        let rcp = 1.0 / length;
        for slot in data.iter_mut() {
            *slot *= rcp;
        }
    }
    length
}

/// `Vector3::crossProduct` (`vector3.ipp:122-133`), the exact component
/// formulas; the `nb_multiply` and `cross2D` arms share them.
fn cross3(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [
        (a[1] * b[2]) - (a[2] * b[1]),
        (a[2] * b[0]) - (a[0] * b[2]),
        (a[0] * b[1]) - (a[1] * b[0]),
    ]
}

/// The lexicographic `operator<` of the reference vector classes, the order
/// `pyVector_richCompareSameWidth` (`script_math.cpp:2464-2491`) compares
/// with.
fn lex_less<const N: usize>(a: [f32; N], b: [f32; N]) -> bool {
    for (left, right) in a.iter().zip(b) {
        if *left != right {
            return *left < right;
        }
    }
    false
}

/// The `Vector3`-only cross product. The reference specialises
/// `PyVector<Vector3>::_py_multiply` (`script_math.cpp:1487`) and the
/// generic width multiplies two vectors as `NotImplemented` (`:1461`), so
/// the non-`Vector3` widths must never name `cross3` (it returns `[f32; 3]`
/// and would not type-check against `[f32; N]`).
macro_rules! vector_cross {
    (cross, $left:expr, $right:expr) => {
        Some(cross3(
            [$left[0], $left[1], $left[2]],
            [$right[0], $right[1], $right[2]],
        ))
    };
    (no_cross, $left:expr, $right:expr) => {
        None
    };
}

/// One width's type: the handlers, the spec and the registration, the
/// reference's `DEFINE_PYVECTOR_TYPEOBJECT(V)` per `V`.
macro_rules! vector_type {
    ($module:ident, $n:expr, $name:expr, $cross:tt) => {
        mod $module {
            use super::*;

            const N: usize = $n;

            /// The one type object of this width, by address: the
            /// instance-type check of `PyVector<V>::Check`.
            static TYPE_ADDR: AtomicUsize = AtomicUsize::new(0);

            pub(super) fn type_addr() -> usize {
                TYPE_ADDR.load(Ordering::SeqCst)
            }

            /// A fresh instance carrying `data`; the caller owns the one
            /// reference. `Script::getData(V)`, the result arm of every
            /// arithmetic handler.
            pub(super) fn make(data: [f32; N]) -> *mut capi::PyObject {
                let Ok(type_object) = binding::create_type(&spec()) else {
                    return std::ptr::null_mut();
                };
                // SAFETY: the interpreter is up (a handler is running) and
                // the handle is one of this module's type objects.
                let Some(instance) = (unsafe { binding::instantiate(&type_object) }) else {
                    return std::ptr::null_mut();
                };
                let pointer = instance.as_ptr();
                // SAFETY: the allocated body is exactly a `VectorBody<N>`,
                // `basicsize` was set to it by `spec`.
                unsafe {
                    std::ptr::addr_of_mut!((*pointer.cast::<VectorBody<N>>()).data).write(data);
                }
                // The reference handed over by `new`; keep the count.
                std::mem::forget(instance);
                pointer
            }

            /// `PyVector<V>::_pyNew` (`script_math.cpp:1284-1303`): a fresh
            /// zero vector, set from the arguments when there are any.
            ///
            /// SAFETY: CPython calls this with this module's type object.
            unsafe extern "C" fn construct(
                type_: *mut capi::PyTypeObject,
                args: *mut capi::PyObject,
                _kwds: *mut capi::PyObject,
            ) -> *mut capi::PyObject {
                // SAFETY: the interpreter is up and the type object is the
                // one being constructed.
                let instance = unsafe { capi::PyType_GenericAlloc(type_, 0) };
                if instance.is_null() {
                    return std::ptr::null_mut();
                }
                // The empty argument list is the zero vector: `_pyNew` only
                // calls `py_set` past zero arguments.
                if unsafe { capi::PyTuple_Size(args) } > 0 {
                    match unsafe { parse_into::<N>(args, type_addr()) } {
                        // SAFETY: the allocated body is a `VectorBody<N>`.
                        Some(data) => unsafe {
                            std::ptr::addr_of_mut!((*instance.cast::<VectorBody<N>>()).data)
                                .write(data);
                        },
                        None => {
                            // SAFETY: the failure left a pending exception;
                            // the allocation's one reference goes back.
                            unsafe { capi::dec_ref(instance) };
                            return std::ptr::null_mut();
                        }
                    }
                }
                instance
            }

            /// `_py_add` (`:1406`): two same-width vectors, else deferred.
            ///
            /// SAFETY: CPython calls this with borrowed operands.
            unsafe extern "C" fn nb_add(
                a: *mut capi::PyObject,
                b: *mut capi::PyObject,
            ) -> *mut capi::PyObject {
                let (Some(left), Some(right)) =
                    (unsafe { as_vector::<N>(a, type_addr()) }, unsafe {
                        as_vector::<N>(b, type_addr())
                    })
                else {
                    return not_implemented();
                };
                make(vec_add::<N>(left, right))
            }

            /// `_py_subtract` (`:1425`).
            ///
            /// SAFETY: CPython calls this with borrowed operands.
            unsafe extern "C" fn nb_subtract(
                a: *mut capi::PyObject,
                b: *mut capi::PyObject,
            ) -> *mut capi::PyObject {
                let (Some(left), Some(right)) =
                    (unsafe { as_vector::<N>(a, type_addr()) }, unsafe {
                        as_vector::<N>(b, type_addr())
                    })
                else {
                    return not_implemented();
                };
                make(vec_sub::<N>(left, right))
            }

            /// `_py_multiply` (`:1461`, the `Vector3` shape at `:1487`):
            /// two `Vector3`s cross, a vector times a float scales -- the
            /// left operand is checked first, so both orders work.
            ///
            /// SAFETY: CPython calls this with borrowed operands.
            unsafe extern "C" fn nb_multiply(
                a: *mut capi::PyObject,
                b: *mut capi::PyObject,
            ) -> *mut capi::PyObject {
                if let Some(left) = unsafe { data_read::<N>(a, type_addr()) } {
                    if let Some(right) = unsafe { data_read::<N>(b, type_addr()) } {
                        // The non-`Vector3` widths never name `right`; keep
                        // the borrow live so the shared macro stays clean.
                        let _ = &right;
                        if let Some(crossed) = vector_cross!($cross, left, right) {
                            return make(crossed);
                        }
                    }
                    if let Some(factor) = unsafe { as_float(b) } {
                        return make(vec_scale::<N>(left, factor));
                    }
                } else if let Some(right) = unsafe { data_read::<N>(b, type_addr()) } {
                    if let Some(factor) = unsafe { as_float(a) } {
                        return make(vec_scale::<N>(right, factor));
                    }
                }
                not_implemented()
            }

            /// `_py_divide` (`:1440`): only the left operand is a vector.
            ///
            /// SAFETY: CPython calls this with borrowed operands.
            unsafe extern "C" fn nb_true_divide(
                a: *mut capi::PyObject,
                b: *mut capi::PyObject,
            ) -> *mut capi::PyObject {
                if let Some(left) = unsafe { data_read::<N>(a, type_addr()) } {
                    if let Some(factor) = unsafe { as_float(b) } {
                        return make(vec_scale::<N>(left, 1.0 / factor));
                    }
                }
                not_implemented()
            }

            /// `py_negative` (`:1520`).
            ///
            /// SAFETY: CPython calls this with a borrowed self.
            unsafe extern "C" fn nb_negative(self_: *mut capi::PyObject) -> *mut capi::PyObject {
                match unsafe { data_read::<N>(self_, type_addr()) } {
                    Some(data) => make(vec_scale::<N>(data, -1.0)),
                    None => std::ptr::null_mut(),
                }
            }

            /// `py_positive` (`:1528`): a fresh copy.
            ///
            /// SAFETY: CPython calls this with a borrowed self.
            unsafe extern "C" fn nb_positive(self_: *mut capi::PyObject) -> *mut capi::PyObject {
                match unsafe { data_read::<N>(self_, type_addr()) } {
                    Some(data) => make(data),
                    None => std::ptr::null_mut(),
                }
            }

            /// `py_nonzero` (`:1534`): `lengthSquared > 0`.
            ///
            /// SAFETY: CPython calls this with a borrowed self.
            unsafe extern "C" fn nb_bool(self_: *mut capi::PyObject) -> c_int {
                match unsafe { data_read::<N>(self_, type_addr()) } {
                    Some(data) => (vec_length_squared::<N>(data) > 0.0) as c_int,
                    None => 0,
                }
            }

            /// `_py_inplace_add` (`:1546`): the operand is a vector shape.
            ///
            /// SAFETY: CPython calls this with borrowed self and operand.
            unsafe extern "C" fn nb_inplace_add(
                self_: *mut capi::PyObject,
                b: *mut capi::PyObject,
            ) -> *mut capi::PyObject {
                let Some(left) = (unsafe { data_read::<N>(self_, type_addr()) }) else {
                    return not_implemented();
                };
                let Some(right) = (unsafe { as_vector::<N>(b, type_addr()) }) else {
                    return not_implemented();
                };
                if !unsafe { data_write::<N>(self_, type_addr(), vec_add::<N>(left, right)) } {
                    return std::ptr::null_mut();
                }
                // SAFETY: the caller borrowed `self_`; the protocol's
                // return is a new reference of it.
                unsafe { capi::inc_ref(self_) };
                self_
            }

            /// `_py_inplace_subtract` (`:1572`).
            ///
            /// SAFETY: CPython calls this with borrowed self and operand.
            unsafe extern "C" fn nb_inplace_subtract(
                self_: *mut capi::PyObject,
                b: *mut capi::PyObject,
            ) -> *mut capi::PyObject {
                let Some(left) = (unsafe { data_read::<N>(self_, type_addr()) }) else {
                    return not_implemented();
                };
                let Some(right) = (unsafe { as_vector::<N>(b, type_addr()) }) else {
                    return not_implemented();
                };
                if !unsafe { data_write::<N>(self_, type_addr(), vec_sub::<N>(left, right)) } {
                    return std::ptr::null_mut();
                }
                // SAFETY: as `nb_inplace_add`.
                unsafe { capi::inc_ref(self_) };
                self_
            }

            /// `_py_inplace_multiply` (`:1598`, the `Vector3` shape at
            /// `:1621`): a `Vector3` operand crosses, a float scales.
            ///
            /// SAFETY: CPython calls this with borrowed self and operand.
            unsafe extern "C" fn nb_inplace_multiply(
                self_: *mut capi::PyObject,
                b: *mut capi::PyObject,
            ) -> *mut capi::PyObject {
                let Some(left) = (unsafe { data_read::<N>(self_, type_addr()) }) else {
                    return not_implemented();
                };
                if let Some(right) = unsafe { data_read::<N>(b, type_addr()) } {
                    // The non-`Vector3` widths never name `right`; keep the
                    // borrow live so the shared macro stays clean.
                    let _ = &right;
                    if let Some(crossed) = vector_cross!($cross, left, right) {
                        if !unsafe { data_write::<N>(self_, type_addr(), crossed) } {
                            return std::ptr::null_mut();
                        }
                        // SAFETY: as `nb_inplace_add`.
                        unsafe { capi::inc_ref(self_) };
                        return self_;
                    }
                }
                let Some(factor) = (unsafe { as_float(b) }) else {
                    return not_implemented();
                };
                if !unsafe { data_write::<N>(self_, type_addr(), vec_scale::<N>(left, factor)) } {
                    return std::ptr::null_mut();
                }
                // SAFETY: as `nb_inplace_add`.
                unsafe { capi::inc_ref(self_) };
                self_
            }

            /// `_py_inplace_divide` (`:1658`): scalar only.
            ///
            /// SAFETY: CPython calls this with borrowed self and operand.
            unsafe extern "C" fn nb_inplace_true_divide(
                self_: *mut capi::PyObject,
                b: *mut capi::PyObject,
            ) -> *mut capi::PyObject {
                let Some(left) = (unsafe { data_read::<N>(self_, type_addr()) }) else {
                    return not_implemented();
                };
                let Some(factor) = (unsafe { as_float(b) }) else {
                    return not_implemented();
                };
                if !unsafe {
                    data_write::<N>(self_, type_addr(), vec_scale::<N>(left, 1.0 / factor))
                } {
                    return std::ptr::null_mut();
                }
                // SAFETY: as `nb_inplace_add`.
                unsafe { capi::inc_ref(self_) };
                self_
            }

            /// `PyVector_tp_richcompare` (`:2500`): same-width pairs get the
            /// lexicographic answer (`:2464`), everything else defers.
            ///
            /// SAFETY: CPython calls this with borrowed operands.
            unsafe extern "C" fn richcompare(
                v: *mut capi::PyObject,
                w: *mut capi::PyObject,
                op: c_int,
            ) -> *mut capi::PyObject {
                let (Some(left), Some(right)) =
                    (unsafe { data_read::<N>(v, type_addr()) }, unsafe {
                        data_read::<N>(w, type_addr())
                    })
                else {
                    return not_implemented();
                };
                let (a_less, b_less) = (lex_less::<N>(left, right), lex_less::<N>(right, left));
                match op {
                    x if x == capi::Py_EQ => bool_ref(!a_less && !b_less),
                    x if x == capi::Py_NE => bool_ref(a_less || b_less),
                    x if x == capi::Py_LT => bool_ref(a_less),
                    x if x == capi::Py_GT => bool_ref(b_less),
                    x if x == capi::Py_LE => bool_ref(!b_less),
                    x if x == capi::Py_GE => bool_ref(!a_less),
                    _ => not_implemented(),
                }
            }

            /// `py_sq_length` (`:1690`): the width.
            ///
            /// SAFETY: CPython calls this with a borrowed self.
            unsafe extern "C" fn sq_length(self_: *mut capi::PyObject) -> capi::PySsize {
                if unsafe { data_read::<N>(self_, type_addr()) }.is_some() {
                    N as capi::PySsize
                } else {
                    0
                }
            }

            /// `py_sq_item` (`:1698`): the indexed component.
            ///
            /// SAFETY: CPython calls this with a borrowed self.
            unsafe extern "C" fn sq_item(
                self_: *mut capi::PyObject,
                index: capi::PySsize,
            ) -> *mut capi::PyObject {
                let Some(data) = (unsafe { data_read::<N>(self_, type_addr()) }) else {
                    return std::ptr::null_mut();
                };
                if index < 0 || index >= N as capi::PySsize {
                    // SAFETY: the interpreter is up (a handler is running).
                    unsafe {
                        capi::PyErr_SetString(
                            capi::PyExc_IndexError,
                            c"Vector index out of range".as_ptr(),
                        );
                    }
                    return std::ptr::null_mut();
                }
                // SAFETY: the bounds were just checked.
                unsafe { capi::PyFloat_FromDouble(data[index as usize] as f64) }
            }

            /// `py_sq_ass_item` (`:1738`): the indexed write.
            ///
            /// SAFETY: CPython calls this with a borrowed self.
            unsafe extern "C" fn sq_ass_item(
                self_: *mut capi::PyObject,
                index: capi::PySsize,
                value: *mut capi::PyObject,
            ) -> c_int {
                let Some(mut data) = (unsafe { data_read::<N>(self_, type_addr()) }) else {
                    return -1;
                };
                if index < 0 || index >= N as capi::PySsize {
                    // SAFETY: the interpreter is up.
                    unsafe {
                        capi::PyErr_SetString(
                            capi::PyExc_IndexError,
                            c"Vector assignment index out of range".as_ptr(),
                        );
                    }
                    return -1;
                }
                let Some(parsed) = (unsafe { as_float(value) }) else {
                    // SAFETY: the interpreter is up.
                    unsafe {
                        capi::PyErr_SetString(
                            capi::PyExc_TypeError,
                            c"Vector element must be a float".as_ptr(),
                        );
                    }
                    return -1;
                };
                data[index as usize] = parsed;
                if !unsafe { data_write::<N>(self_, type_addr(), data) } {
                    return -1;
                }
                0
            }

            /// `py_set` (`:1801`), the method face of `parse_into`.
            ///
            /// SAFETY: CPython calls this with borrowed self and args.
            unsafe extern "C" fn set(
                self_: *mut capi::PyObject,
                args: *mut capi::PyObject,
            ) -> *mut capi::PyObject {
                match unsafe { parse_into::<N>(args, type_addr()) } {
                    Some(data) => {
                        if !unsafe { data_write::<N>(self_, type_addr(), data) } {
                            return std::ptr::null_mut();
                        }
                        none_ref()
                    }
                    None => std::ptr::null_mut(),
                }
            }

            /// `py_scale` (`:1860`): a scaled copy, not a mutation.
            ///
            /// SAFETY: CPython calls this with borrowed self and args.
            unsafe extern "C" fn scale(
                self_: *mut capi::PyObject,
                args: *mut capi::PyObject,
            ) -> *mut capi::PyObject {
                let fail = || -> *mut capi::PyObject {
                    // SAFETY: the interpreter is up.
                    unsafe {
                        capi::PyErr_SetString(
                            capi::PyExc_TypeError,
                            c"Vector.scale expects a float argument".as_ptr(),
                        );
                    }
                    std::ptr::null_mut()
                };
                let Some(data) = (unsafe { data_read::<N>(self_, type_addr()) }) else {
                    return std::ptr::null_mut();
                };
                // SAFETY: the size read is on the borrowed args tuple.
                if unsafe { capi::PyTuple_Size(args) } != 1 {
                    return fail();
                }
                // SAFETY: the index is inside the checked tuple.
                let item = unsafe { capi::PyTuple_GetItem(args, 0) };
                let Some(factor) = (unsafe { as_float(item) }) else {
                    return fail();
                };
                make(vec_scale::<N>(data, factor))
            }

            /// `py_dot` (`:1888`): the argument table is the constructor's.
            ///
            /// SAFETY: CPython calls this with borrowed self and args.
            unsafe extern "C" fn dot(
                self_: *mut capi::PyObject,
                args: *mut capi::PyObject,
            ) -> *mut capi::PyObject {
                let Some(data) = (unsafe { data_read::<N>(self_, type_addr()) }) else {
                    return std::ptr::null_mut();
                };
                match unsafe { parse_into::<N>(args, type_addr()) } {
                    Some(other) => {
                        // SAFETY: the interpreter is up.
                        unsafe { capi::PyFloat_FromDouble(vec_dot::<N>(data, other) as f64) }
                    }
                    None => std::ptr::null_mut(),
                }
            }

            /// `py_normalise` (`:1915`): in place, no arguments.
            ///
            /// SAFETY: CPython calls this with borrowed self and args.
            unsafe extern "C" fn normalise(
                self_: *mut capi::PyObject,
                args: *mut capi::PyObject,
            ) -> *mut capi::PyObject {
                // SAFETY: the size read is on the borrowed args tuple.
                if unsafe { capi::PyTuple_Size(args) } != 0 {
                    // SAFETY: the interpreter is up.
                    unsafe {
                        capi::PyErr_SetString(
                            capi::PyExc_TypeError,
                            c"Vector.normalise takes no arguments\
                              (nor does it brook any dissent :)"
                                .as_ptr(),
                        );
                    }
                    return std::ptr::null_mut();
                }
                let Some(mut data) = (unsafe { data_read::<N>(self_, type_addr()) }) else {
                    return std::ptr::null_mut();
                };
                vec_normalise::<N>(&mut data);
                if !unsafe { data_write::<N>(self_, type_addr(), data) } {
                    return std::ptr::null_mut();
                }
                none_ref()
            }

            /// `py_tuple` (`:1938`).
            ///
            /// SAFETY: CPython calls this with borrowed self and args.
            unsafe extern "C" fn tuple(
                self_: *mut capi::PyObject,
                args: *mut capi::PyObject,
            ) -> *mut capi::PyObject {
                // SAFETY: the size read is on the borrowed args tuple.
                if unsafe { capi::PyTuple_Size(args) } != 0 {
                    // SAFETY: the interpreter is up.
                    unsafe {
                        capi::PyErr_SetString(
                            capi::PyExc_TypeError,
                            c"Vector.tuple takes no arguments".as_ptr(),
                        );
                    }
                    return std::ptr::null_mut();
                }
                let Some(data) = (unsafe { data_read::<N>(self_, type_addr()) }) else {
                    return std::ptr::null_mut();
                };
                // SAFETY: the interpreter is up; every item is a fresh
                // float whose reference the tuple takes over.
                unsafe {
                    let out = capi::PyTuple_New(N as capi::PySsize);
                    if out.is_null() {
                        return std::ptr::null_mut();
                    }
                    for (index, value) in data.iter().enumerate() {
                        capi::PyTuple_SetItem(
                            out,
                            index as capi::PySsize,
                            capi::PyFloat_FromDouble(*value as f64),
                        );
                    }
                    out
                }
            }

            /// `py_list` (`:1950`).
            ///
            /// SAFETY: CPython calls this with borrowed self and args.
            unsafe extern "C" fn list(
                self_: *mut capi::PyObject,
                args: *mut capi::PyObject,
            ) -> *mut capi::PyObject {
                // SAFETY: the size read is on the borrowed args tuple.
                if unsafe { capi::PyTuple_Size(args) } != 0 {
                    // SAFETY: the interpreter is up.
                    unsafe {
                        capi::PyErr_SetString(
                            capi::PyExc_TypeError,
                            c"Vector.list takes no arguments".as_ptr(),
                        );
                    }
                    return std::ptr::null_mut();
                }
                let Some(data) = (unsafe { data_read::<N>(self_, type_addr()) }) else {
                    return std::ptr::null_mut();
                };
                // SAFETY: as `tuple`.
                unsafe {
                    let out = capi::PyList_New(N as capi::PySsize);
                    if out.is_null() {
                        return std::ptr::null_mut();
                    }
                    for (index, value) in data.iter().enumerate() {
                        capi::PyList_SetItem(
                            out,
                            index as capi::PySsize,
                            capi::PyFloat_FromDouble(*value as f64),
                        );
                    }
                    out
                }
            }

            /// `py_cross2D` (`:1984`, the `Vector2` shape at `:1996`, the
            /// `Vector3` shape at `:2011`); every other width keeps the
            /// refusal (`:1988`).
            ///
            /// SAFETY: CPython calls this with borrowed self and args.
            #[allow(clippy::out_of_bounds_indexing)]
            unsafe extern "C" fn cross2d(
                self_: *mut capi::PyObject,
                args: *mut capi::PyObject,
            ) -> *mut capi::PyObject {
                let refuse = || -> *mut capi::PyObject {
                    // SAFETY: the interpreter is up.
                    unsafe {
                        capi::PyErr_SetString(
                            capi::PyExc_TypeError,
                            c"No cross2D for this type of vector".as_ptr(),
                        );
                    }
                    std::ptr::null_mut()
                };
                let Some(data) = (unsafe { data_read::<N>(self_, type_addr()) }) else {
                    return std::ptr::null_mut();
                };
                // The generic width refuses before it reads the arguments.
                if N != 2 && N != 3 {
                    return refuse();
                }
                let Some(other) = (unsafe { parse_into::<N>(args, type_addr()) }) else {
                    return std::ptr::null_mut();
                };
                let result = if N == 2 {
                    // `thisV[0] * otherV[1] - thisV[1] * otherV[0]`.
                    data[0] * other[1] - data[1] * other[0]
                } else {
                    // `thisV[0] * otherV[2] - thisV[2] * otherV[0]`.
                    data[0] * other[2] - data[2] * other[0]
                };
                // SAFETY: the interpreter is up.
                unsafe { capi::PyFloat_FromDouble(result as f64) }
            }

            /// `py_distTo` (`:2037`).
            ///
            /// SAFETY: CPython calls this with borrowed self and args.
            unsafe extern "C" fn dist_to(
                self_: *mut capi::PyObject,
                args: *mut capi::PyObject,
            ) -> *mut capi::PyObject {
                let Some(data) = (unsafe { data_read::<N>(self_, type_addr()) }) else {
                    return std::ptr::null_mut();
                };
                match unsafe { parse_operand::<N>(args, type_addr()) } {
                    Some(other) => {
                        // SAFETY: the interpreter is up.
                        unsafe { capi::PyFloat_FromDouble(vec_distance::<N>(data, other) as f64) }
                    }
                    None => std::ptr::null_mut(),
                }
            }

            /// `py_distSqrTo` (`:2053`).
            ///
            /// SAFETY: CPython calls this with borrowed self and args.
            unsafe extern "C" fn dist_sqr_to(
                self_: *mut capi::PyObject,
                args: *mut capi::PyObject,
            ) -> *mut capi::PyObject {
                let Some(data) = (unsafe { data_read::<N>(self_, type_addr()) }) else {
                    return std::ptr::null_mut();
                };
                match unsafe { parse_operand::<N>(args, type_addr()) } {
                    Some(other) => {
                        // SAFETY: the interpreter is up.
                        unsafe {
                            capi::PyFloat_FromDouble(vec_distance_squared::<N>(data, other) as f64)
                        }
                    }
                    None => std::ptr::null_mut(),
                }
            }

            /// `py_flatDistTo` (`:2065`, the `Vector3` shape at `:2077`).
            ///
            /// SAFETY: CPython calls this with borrowed self and args.
            #[allow(clippy::out_of_bounds_indexing)]
            unsafe extern "C" fn flat_dist_to(
                self_: *mut capi::PyObject,
                args: *mut capi::PyObject,
            ) -> *mut capi::PyObject {
                let refuse = || -> *mut capi::PyObject {
                    // SAFETY: the interpreter is up.
                    unsafe {
                        capi::PyErr_SetString(
                            capi::PyExc_TypeError,
                            c"No flatDistTo for this type of vector".as_ptr(),
                        );
                    }
                    std::ptr::null_mut()
                };
                if N != 3 {
                    return refuse();
                }
                let Some(data) = (unsafe { data_read::<N>(self_, type_addr()) }) else {
                    return std::ptr::null_mut();
                };
                match unsafe { parse_operand::<N>(args, type_addr()) } {
                    Some(other) => {
                        let (x, z) = (data[0] - other[0], data[2] - other[2]);
                        // SAFETY: the interpreter is up.
                        unsafe { capi::PyFloat_FromDouble((x * x + z * z).sqrt() as f64) }
                    }
                    None => std::ptr::null_mut(),
                }
            }

            /// `py_flatDistSqrTo` (`:2102`, the `Vector3` shape at `:2114`).
            ///
            /// SAFETY: CPython calls this with borrowed self and args.
            #[allow(clippy::out_of_bounds_indexing)]
            unsafe extern "C" fn flat_dist_sqr_to(
                self_: *mut capi::PyObject,
                args: *mut capi::PyObject,
            ) -> *mut capi::PyObject {
                let refuse = || -> *mut capi::PyObject {
                    // SAFETY: the interpreter is up.
                    unsafe {
                        capi::PyErr_SetString(
                            capi::PyExc_TypeError,
                            c"No flatDistSqrTo for this type of vector".as_ptr(),
                        );
                    }
                    std::ptr::null_mut()
                };
                if N != 3 {
                    return refuse();
                }
                let Some(data) = (unsafe { data_read::<N>(self_, type_addr()) }) else {
                    return std::ptr::null_mut();
                };
                match unsafe { parse_operand::<N>(args, type_addr()) } {
                    Some(other) => {
                        let (x, z) = (data[0] - other[0], data[2] - other[2]);
                        // SAFETY: the interpreter is up.
                        unsafe { capi::PyFloat_FromDouble((x * x + z * z) as f64) }
                    }
                    None => std::ptr::null_mut(),
                }
            }

            /// `py_setPitchYaw` (`:2139`, the `Vector3` shape at `:2152`).
            ///
            /// SAFETY: CPython calls this with borrowed self and args.
            #[allow(clippy::out_of_bounds_indexing)]
            unsafe extern "C" fn set_pitch_yaw(
                self_: *mut capi::PyObject,
                args: *mut capi::PyObject,
            ) -> *mut capi::PyObject {
                let refuse = || -> *mut capi::PyObject {
                    // SAFETY: the interpreter is up.
                    unsafe {
                        capi::PyErr_SetString(
                            capi::PyExc_TypeError,
                            c"No setPitchYaw for this type of vector".as_ptr(),
                        );
                    }
                    std::ptr::null_mut()
                };
                if N != 3 {
                    return refuse();
                }
                // SAFETY: the size read is on the borrowed args tuple.
                if unsafe { capi::PyTuple_Size(args) } != 2 {
                    // SAFETY: the interpreter is up.
                    unsafe {
                        capi::PyErr_SetString(
                            capi::PyExc_TypeError,
                            c"Vector3.setPitchYaw expects exactly two arguments".as_ptr(),
                        );
                    }
                    return std::ptr::null_mut();
                }
                // SAFETY: the indices are inside the checked tuple.
                let (pitch, yaw) = unsafe {
                    (
                        capi::PyTuple_GetItem(args, 0),
                        capi::PyTuple_GetItem(args, 1),
                    )
                };
                let (Some(pitch), Some(yaw)) =
                    (unsafe { as_float(pitch) }, unsafe { as_float(yaw) })
                else {
                    // SAFETY: the interpreter is up; a float was refused.
                    unsafe {
                        capi::PyErr_SetString(
                            capi::PyExc_TypeError,
                            c"Vector3.setPitchYaw expects floats".as_ptr(),
                        );
                    }
                    return std::ptr::null_mut();
                };
                let Some(data) = (unsafe { data_read::<N>(self_, type_addr()) }) else {
                    return std::ptr::null_mut();
                };
                let mut set = foundation::math::Vector3::new(data[0], data[1], data[2]);
                set.set_pitch_yaw(pitch, yaw);
                // The width branch is dead off `Vector3`; the component
                // writes go through the generic body for the other widths.
                let mut written = [0.0f32; N];
                written[0] = set.x;
                written[1] = set.y;
                written[2] = set.z;
                if !unsafe { data_write::<N>(self_, type_addr(), written) } {
                    return std::ptr::null_mut();
                }
                none_ref()
            }

            /// `pyGet_x` (`:2255`); the rest of the component getters share
            /// the shape.
            ///
            /// SAFETY: CPython calls this with a borrowed self.
            unsafe extern "C" fn get_x(
                self_: *mut capi::PyObject,
                _closure: *mut std::os::raw::c_void,
            ) -> *mut capi::PyObject {
                (unsafe { component(self_, 0) })
            }

            /// `pyGet_y` (`:2262`).
            ///
            /// SAFETY: CPython calls this with a borrowed self.
            unsafe extern "C" fn get_y(
                self_: *mut capi::PyObject,
                _closure: *mut std::os::raw::c_void,
            ) -> *mut capi::PyObject {
                (unsafe { component(self_, 1) })
            }

            /// `pyGet_z` (`:2269`).
            ///
            /// SAFETY: CPython calls this with a borrowed self.
            unsafe extern "C" fn get_z(
                self_: *mut capi::PyObject,
                _closure: *mut std::os::raw::c_void,
            ) -> *mut capi::PyObject {
                (unsafe { component(self_, 2) })
            }

            /// `pyGet_w` (`:2277`).
            ///
            /// SAFETY: CPython calls this with a borrowed self.
            unsafe extern "C" fn get_w(
                self_: *mut capi::PyObject,
                _closure: *mut std::os::raw::c_void,
            ) -> *mut capi::PyObject {
                (unsafe { component(self_, 3) })
            }

            /// One component read shared by the getters.
            ///
            /// SAFETY: CPython calls the getters with a borrowed self.
            unsafe fn component(self_: *mut capi::PyObject, index: usize) -> *mut capi::PyObject {
                let Some(data) = (unsafe { data_read::<N>(self_, type_addr()) }) else {
                    return std::ptr::null_mut();
                };
                if index >= N {
                    return std::ptr::null_mut();
                }
                // SAFETY: the interpreter is up.
                unsafe { capi::PyFloat_FromDouble(data[index] as f64) }
            }

            /// `pySet_x` (`:2285`); the rest of the setters share the
            /// shape.
            ///
            /// SAFETY: CPython calls this with a borrowed self.
            unsafe extern "C" fn set_x(
                self_: *mut capi::PyObject,
                value: *mut capi::PyObject,
                _closure: *mut std::os::raw::c_void,
            ) -> c_int {
                (unsafe { assign(self_, 0, value) })
            }

            /// `pySet_y` (`:2299`).
            ///
            /// SAFETY: CPython calls this with a borrowed self.
            unsafe extern "C" fn set_y(
                self_: *mut capi::PyObject,
                value: *mut capi::PyObject,
                _closure: *mut std::os::raw::c_void,
            ) -> c_int {
                (unsafe { assign(self_, 1, value) })
            }

            /// `pySet_z` (`:2313`).
            ///
            /// SAFETY: CPython calls this with a borrowed self.
            unsafe extern "C" fn set_z(
                self_: *mut capi::PyObject,
                value: *mut capi::PyObject,
                _closure: *mut std::os::raw::c_void,
            ) -> c_int {
                (unsafe { assign(self_, 2, value) })
            }

            /// `pySet_w` (`:2327`).
            ///
            /// SAFETY: CPython calls this with a borrowed self.
            unsafe extern "C" fn set_w(
                self_: *mut capi::PyObject,
                value: *mut capi::PyObject,
                _closure: *mut std::os::raw::c_void,
            ) -> c_int {
                (unsafe { assign(self_, 3, value) })
            }

            /// One component write shared by the setters.
            ///
            /// SAFETY: CPython calls the setters with a borrowed self.
            unsafe fn assign(
                self_: *mut capi::PyObject,
                index: usize,
                value: *mut capi::PyObject,
            ) -> c_int {
                let Some(mut data) = (unsafe { data_read::<N>(self_, type_addr()) }) else {
                    return -1;
                };
                if index >= N {
                    return -1;
                }
                let Some(parsed) = (unsafe { as_float(value) }) else {
                    // SAFETY: the interpreter is up.
                    unsafe {
                        capi::PyErr_SetString(
                            capi::PyExc_TypeError,
                            c"Vector component must be a float".as_ptr(),
                        );
                    }
                    return -1;
                };
                data[index] = parsed;
                if !unsafe { data_write::<N>(self_, type_addr(), data) } {
                    return -1;
                }
                0
            }

            /// `pyGet_isReference` (`PY_ATTRIBUTE` table, `:2586`): the
            /// script-facing vectors are values, never references.
            ///
            /// SAFETY: CPython calls this with a borrowed self.
            unsafe extern "C" fn get_is_reference(
                _self_: *mut capi::PyObject,
                _closure: *mut std::os::raw::c_void,
            ) -> *mut capi::PyObject {
                bool_ref(false)
            }

            /// `pyGet_isReadOnly`: the constructed vectors are writable.
            ///
            /// SAFETY: CPython calls this with a borrowed self.
            unsafe extern "C" fn get_is_read_only(
                _self_: *mut capi::PyObject,
                _closure: *mut std::os::raw::c_void,
            ) -> *mut capi::PyObject {
                bool_ref(false)
            }

            /// `pyGet_length`: `getVector().length()`.
            ///
            /// SAFETY: CPython calls this with a borrowed self.
            unsafe extern "C" fn get_length(
                self_: *mut capi::PyObject,
                _closure: *mut std::os::raw::c_void,
            ) -> *mut capi::PyObject {
                let Some(data) = (unsafe { data_read::<N>(self_, type_addr()) }) else {
                    return std::ptr::null_mut();
                };
                // SAFETY: the interpreter is up.
                unsafe { capi::PyFloat_FromDouble(vec_length::<N>(data) as f64) }
            }

            /// `pyGet_lengthSquared`.
            ///
            /// SAFETY: CPython calls this with a borrowed self.
            unsafe extern "C" fn get_length_squared(
                self_: *mut capi::PyObject,
                _closure: *mut std::os::raw::c_void,
            ) -> *mut capi::PyObject {
                let Some(data) = (unsafe { data_read::<N>(self_, type_addr()) }) else {
                    return std::ptr::null_mut();
                };
                // SAFETY: the interpreter is up.
                unsafe { capi::PyFloat_FromDouble(vec_length_squared::<N>(data) as f64) }
            }

            /// `pyGet_yaw` (`:2227`, the `Vector3` shape at `:2233`); the
            /// other widths return `None`.
            ///
            /// SAFETY: CPython calls this with a borrowed self.
            #[allow(clippy::out_of_bounds_indexing)]
            unsafe extern "C" fn get_yaw(
                self_: *mut capi::PyObject,
                _closure: *mut std::os::raw::c_void,
            ) -> *mut capi::PyObject {
                if N != 3 {
                    return none_ref();
                }
                let Some(data) = (unsafe { data_read::<N>(self_, type_addr()) }) else {
                    return std::ptr::null_mut();
                };
                let yaw = foundation::math::Vector3::new(data[0], data[1], data[2]).yaw();
                // SAFETY: the interpreter is up.
                unsafe { capi::PyFloat_FromDouble(yaw as f64) }
            }

            /// `pyGet_pitch` (`:2242`, the `Vector3` shape at `:2248`).
            ///
            /// SAFETY: CPython calls this with a borrowed self.
            #[allow(clippy::out_of_bounds_indexing)]
            unsafe extern "C" fn get_pitch(
                self_: *mut capi::PyObject,
                _closure: *mut std::os::raw::c_void,
            ) -> *mut capi::PyObject {
                if N != 3 {
                    return none_ref();
                }
                let Some(data) = (unsafe { data_read::<N>(self_, type_addr()) }) else {
                    return std::ptr::null_mut();
                };
                let pitch = foundation::math::Vector3::new(data[0], data[1], data[2]).pitch();
                // SAFETY: the interpreter is up.
                unsafe { capi::PyFloat_FromDouble(pitch as f64) }
            }

            /// The `PY_BEGIN_METHODS` table (`:2549-2580`) and the
            /// `PY_BEGIN_ATTRIBUTES` table (`:2584-2610`) of one width, the
            /// port's builder shape of the reference's macro pair.
            pub(super) fn spec() -> binding::TypeSpec {
                let spec = binding::TypeSpec::new($name)
                    .with_new(construct)
                    .sequence(sq_length, sq_item, sq_ass_item)
                    .richcompare(Some(richcompare))
                    .number(
                        Some(nb_add),
                        Some(nb_subtract),
                        Some(nb_multiply),
                        Some(nb_true_divide),
                        Some(nb_negative),
                        Some(nb_positive),
                        Some(nb_bool),
                        Some(nb_inplace_add),
                        Some(nb_inplace_subtract),
                        Some(nb_inplace_multiply),
                        Some(nb_inplace_true_divide),
                    )
                    .attribute("x", get_x, Some(set_x))
                    .attribute("y", get_y, Some(set_y))
                    .attribute("isReference", get_is_reference, None)
                    .attribute("isReadOnly", get_is_read_only, None)
                    .attribute("length", get_length, None)
                    .attribute("lengthSquared", get_length_squared, None)
                    .method("set", set)
                    .method("scale", scale)
                    .method("dot", dot)
                    .method("normalise", normalise)
                    .method("tuple", tuple)
                    .method("list", list)
                    .method("cross2D", cross2d)
                    .method("distTo", dist_to)
                    .method("distSqrTo", dist_sqr_to)
                    .method("flatDistTo", flat_dist_to)
                    .method("flatDistSqrTo", flat_dist_sqr_to)
                    .method("setPitchYaw", set_pitch_yaw)
                    .instance_size(instance_size::<N>());
                let spec = if N >= 3 {
                    spec.attribute("z", get_z, Some(set_z))
                } else {
                    spec
                };
                let spec = if N >= 4 {
                    spec.attribute("w", get_w, Some(set_w))
                } else {
                    spec
                };
                if N == 3 {
                    spec.attribute("yaw", get_yaw, None)
                        .attribute("pitch", get_pitch, None)
                } else {
                    spec
                }
            }

            /// `PyVector_init` (`script_math.cpp:3043-3049`): the one type
            /// object, then into the `Math` module dictionary the way
            /// `PyModule_AddObject` lands there.
            pub(super) fn register() -> Result<PyRef, binding::BindError> {
                let type_object = binding::create_type(&spec())?;
                TYPE_ADDR.store(type_object.as_ptr() as usize, Ordering::SeqCst);
                // SAFETY: the interpreter is up (`register` runs from init)
                // and the handle is one of this module's type objects.
                unsafe {
                    let module = capi::PyImport_AddModule(c"Math".as_ptr());
                    if !module.is_null() {
                        let dict = capi::PyModule_GetDict(module);
                        let name = std::ffi::CString::new(&$name["Math.".len()..])
                            .expect("a static dotted name has no NUL");
                        capi::PyDict_SetItemString(dict, name.as_ptr(), type_object.as_ptr());
                    }
                }
                // The dictionary took its own reference; this one goes back.
                Ok(type_object)
            }
        }
    };
}

vector_type!(vector2, 2, "Math.Vector2", no_cross);
vector_type!(vector3, 3, "Math.Vector3", cross);
vector_type!(vector4, 4, "Math.Vector4", no_cross);

/// The `Math.Matrix` and `Math.MatrixProvider` types
/// (`script_math.cpp:740-...`): `MatrixProvider` is the abstract base every
/// matrix-carrying script object derives from, and `PyMatrix` is the concrete
/// 4x4 value the scripts mutate directly.
///
/// The reference's `MatrixProvider::matrix(Matrix&)` is a virtual that each
/// subclass fills in; the port dispatches on the concrete type address in
/// [`provider_matrix`], the same dispatch the reference's vtable performs.
mod matrix {
    use super::*;
    use foundation::math::Matrix;
    use std::collections::BTreeMap;
    use std::sync::Mutex;

    /// The instance body beyond the `PyObject` header: the 16 f32s of the
    /// matrix inline, the layout of the reference's `PyMatrix` (which embeds
    /// a `Matrix`).
    #[repr(C)]
    struct MatrixBody {
        header: capi::PyObject,
        m: [[f32; 4]; 4],
    }

    const INSTANCE_SIZE: usize =
        std::mem::size_of::<MatrixBody>() - std::mem::size_of::<capi::PyObject>();

    /// The one `Math.Matrix` type object, by address.
    static TYPE_ADDR: AtomicUsize = AtomicUsize::new(0);
    /// The one `Math.MatrixProvider` base type object, by address.
    static PROVIDER_ADDR: AtomicUsize = AtomicUsize::new(0);

    fn type_addr() -> usize {
        TYPE_ADDR.load(Ordering::SeqCst)
    }

    /// The base type object address, the derived providers' `Py_tp_base`.
    pub(super) fn provider_addr() -> usize {
        PROVIDER_ADDR.load(Ordering::SeqCst)
    }

    fn matrix_read(object: *mut capi::PyObject) -> Option<Matrix> {
        if object.is_null() {
            return None;
        }
        // SAFETY: the type check gates the body read to `MatrixBody`.
        unsafe {
            if capi::Py_TYPE(object) as usize != type_addr() {
                return None;
            }
            Some(Matrix {
                m: std::ptr::addr_of!((*object.cast::<MatrixBody>()).m).read(),
            })
        }
    }

    fn matrix_write(object: *mut capi::PyObject, m: &Matrix) -> bool {
        if object.is_null() {
            return false;
        }
        // SAFETY: the type check gates the body write to `MatrixBody`.
        unsafe {
            if capi::Py_TYPE(object) as usize != type_addr() {
                return false;
            }
            std::ptr::addr_of_mut!((*object.cast::<MatrixBody>()).m).write(m.m);
        }
        true
    }

    /// A fresh `Math.Matrix` carrying `m`; the caller owns the reference.
    fn make(m: &Matrix) -> *mut capi::PyObject {
        let Ok(type_object) = binding::create_type(&spec()) else {
            return std::ptr::null_mut();
        };
        let Some(instance) = (unsafe { binding::instantiate(&type_object) }) else {
            return std::ptr::null_mut();
        };
        let pointer = instance.as_ptr();
        // SAFETY: the allocated body is exactly a `MatrixBody`.
        unsafe {
            std::ptr::addr_of_mut!((*pointer.cast::<MatrixBody>()).m).write(m.m);
        }
        std::mem::forget(instance);
        pointer
    }

    /// `MatrixProvider::Check` (`script_math.cpp:785`): any object whose type
    /// is `MatrixProvider` or one of its subclasses. The `MatrixProvider`
    /// slot family and the derived providers call it, so it leaves the
    /// module.
    pub(super) fn provider_check(object: *mut capi::PyObject) -> bool {
        let provider = provider_addr();
        if object.is_null() || provider == 0 {
            return false;
        }
        // SAFETY: the registry only stores type objects `create_type` built.
        unsafe {
            capi::PyType_IsSubtype(capi::Py_TYPE(object), provider as *mut capi::PyTypeObject) == 1
        }
    }

    // -- the `matrix()` registry: the reference's virtual dispatch, filled by
    //    the providers that compute rather than store (`Vector4MatrixAdaptor`,
    //    the matrix products) --

    /// A computed provider's `matrix`: the fills-the-out-argument virtual.
    type MatrixOutput = fn(*mut capi::PyObject) -> Matrix;

    /// The computed providers' `matrix`s, keyed by type object address.
    static MATRIX_OUTPUT: Mutex<BTreeMap<usize, MatrixOutput>> = Mutex::new(BTreeMap::new());

    /// Record one computed provider's `matrix` under its type object address.
    /// Called from the provider's `register`, before a script can run.
    pub(super) fn register_matrix_output(type_addr: usize, output: MatrixOutput) {
        MATRIX_OUTPUT
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(type_addr, output);
    }

    /// The matrix a `MatrixProvider` yields. The dispatch is the reference's
    /// virtual `matrix()`: a `PyMatrix` reads its embedded body, a computed
    /// provider fills the out argument, and anything else is not a provider
    /// at all. An empty computed slot (no source) keeps the reference's
    /// "leave the out argument untouched" contract -- the caller's
    /// default-constructed matrix, the zero matrix.
    pub(super) fn provider_matrix(object: *mut capi::PyObject) -> Option<Matrix> {
        if !provider_check(object) {
            return None;
        }
        let output = {
            let table = MATRIX_OUTPUT
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            table
                .get(&unsafe { capi::Py_TYPE(object) as usize })
                .copied()
        };
        match output {
            Some(output) => Some(output(object)),
            None => matrix_read(object),
        }
    }

    // -- the `MatrixProvider` slot family, shared by the providers that hold
    //    one (`Vector4Translation`, `Vector4Distance`, the matrix products) --

    /// The stored provider of a slot, by byte offset from the object start.
    ///
    /// # Safety
    ///
    /// `self_` is a borrowed instance of the slot's owner type, and `FIELD`
    /// is that body's offset of the slot.
    pub(super) unsafe fn slot_read<const FIELD: usize>(
        self_: *mut capi::PyObject,
    ) -> *mut capi::PyObject {
        unsafe {
            std::ptr::addr_of!(*self_.cast::<u8>().add(FIELD).cast::<*mut capi::PyObject>()).read()
        }
    }

    /// Store `value` in the slot. The caller releases the previous occupant
    /// first, the reference's `SmartPointer` assignment half.
    ///
    /// # Safety
    ///
    /// As [`slot_read`].
    pub(super) unsafe fn slot_write<const FIELD: usize>(
        self_: *mut capi::PyObject,
        value: *mut capi::PyObject,
    ) {
        unsafe {
            std::ptr::addr_of_mut!(*self_.cast::<u8>().add(FIELD).cast::<*mut capi::PyObject>())
                .write(value)
        }
    }

    /// `Script::getData( MatrixProviderPtr )` (`script.hpp:604-608`) on a
    /// held slot: the stored provider, or `None` for an empty slot.
    ///
    /// # Safety
    ///
    /// `self_` is a borrowed instance of the slot's owner type.
    pub(super) unsafe extern "C" fn slot_get<const FIELD: usize>(
        self_: *mut capi::PyObject,
        _closure: *mut std::os::raw::c_void,
    ) -> *mut capi::PyObject {
        let stored = unsafe { slot_read::<FIELD>(self_) };
        let object = if stored.is_null() {
            capi::py_none()
        } else {
            stored
        };
        // SAFETY: the slot's occupant is a borrowed reference the read does
        // not consume; the caller gets its own.
        unsafe {
            capi::inc_ref(object);
        }
        object
    }

    /// `Script::setData( pObject, MatrixProviderPtr&, varName )`
    /// (`script.hpp:577-631`): a `MatrixProvider`, or `None` to empty the
    /// slot; the field owns the reference the way the reference's
    /// `SmartPointer` does, the previous occupant released before the new
    /// one is stored, and a self-assignment a no-op. A null assignment is
    /// refused, the reference's `coerce`-then-`Check` path.
    ///
    /// # Safety
    ///
    /// As [`slot_get`].
    pub(super) unsafe extern "C" fn slot_set<const FIELD: usize>(
        self_: *mut capi::PyObject,
        value: *mut capi::PyObject,
        closure: *mut std::os::raw::c_void,
    ) -> c_int {
        if value.is_null() {
            type_error(&format!(
                "{} must be set to a MatrixProvider or None",
                attribute_name(closure)
            ));
            return -1;
        }
        if value == capi::py_none() {
            // `pCoerced == Py_None`: the slot is emptied.
            let old = unsafe { slot_read::<FIELD>(self_) };
            if !old.is_null() {
                unsafe { capi::dec_ref(old) };
            }
            unsafe { slot_write::<FIELD>(self_, std::ptr::null_mut()) };
            return 0;
        }
        if !provider_check(value) {
            type_error(&format!(
                "{} must be set to a MatrixProvider or None",
                attribute_name(closure)
            ));
            return -1;
        }
        let old = unsafe { slot_read::<FIELD>(self_) };
        if old != value {
            if !old.is_null() {
                unsafe { capi::dec_ref(old) };
            }
            // The slot takes over its own reference, the assignment's
            // `SmartPointer` incref half.
            unsafe {
                capi::inc_ref(value);
                slot_write::<FIELD>(self_, value);
            }
        }
        0
    }

    // -- the method handlers, one per `PY_AUTO_METHOD_DECLARE` in
    //    `script_math.hpp:171-232` -------------------------------------------

    unsafe extern "C" fn set(
        self_: *mut capi::PyObject,
        args: *mut capi::PyObject,
    ) -> *mut capi::PyObject {
        if !want_args(args, 1) {
            return type_error("Matrix.set expects 1 argument");
        }
        let provider = arg(args, 0);
        let Some(m) = provider_matrix(provider) else {
            return type_error("Matrix.set expects a MatrixProvider");
        };
        matrix_write(self_, &m);
        none_ref()
    }

    unsafe extern "C" fn set_zero(
        self_: *mut capi::PyObject,
        args: *mut capi::PyObject,
    ) -> *mut capi::PyObject {
        if !want_args(args, 0) {
            return type_error("Matrix.setZero takes no arguments");
        }
        matrix_write(self_, &Matrix::zero());
        none_ref()
    }

    unsafe extern "C" fn set_identity(
        self_: *mut capi::PyObject,
        args: *mut capi::PyObject,
    ) -> *mut capi::PyObject {
        if !want_args(args, 0) {
            return type_error("Matrix.setIdentity takes no arguments");
        }
        matrix_write(self_, &Matrix::identity());
        none_ref()
    }

    unsafe extern "C" fn set_scale(
        self_: *mut capi::PyObject,
        args: *mut capi::PyObject,
    ) -> *mut capi::PyObject {
        if !want_args(args, 1) {
            return type_error("Matrix.setScale expects 1 argument");
        }
        let Some(v) = parse_vec3(arg(args, 0)) else {
            return type_error("Matrix.setScale expects a Vector3");
        };
        let mut m = Matrix::zero();
        m.set_scale(v.x, v.y, v.z);
        matrix_write(self_, &m);
        none_ref()
    }

    unsafe extern "C" fn set_translate(
        self_: *mut capi::PyObject,
        args: *mut capi::PyObject,
    ) -> *mut capi::PyObject {
        if !want_args(args, 1) {
            return type_error("Matrix.setTranslate expects 1 argument");
        }
        let Some(v) = parse_vec3(arg(args, 0)) else {
            return type_error("Matrix.setTranslate expects a Vector3");
        };
        let mut m = Matrix::zero();
        m.set_translate(v.x, v.y, v.z);
        matrix_write(self_, &m);
        none_ref()
    }

    unsafe extern "C" fn set_rotate_x(
        self_: *mut capi::PyObject,
        args: *mut capi::PyObject,
    ) -> *mut capi::PyObject {
        if !want_args(args, 1) {
            return type_error("Matrix.setRotateX expects 1 argument");
        }
        let angle = match parse_angle(arg(args, 0)) {
            Ok(a) => a,
            Err(e) => return e,
        };
        let mut m = Matrix::zero();
        m.set_rotate_x(angle);
        matrix_write(self_, &m);
        none_ref()
    }

    unsafe extern "C" fn set_rotate_y(
        self_: *mut capi::PyObject,
        args: *mut capi::PyObject,
    ) -> *mut capi::PyObject {
        if !want_args(args, 1) {
            return type_error("Matrix.setRotateY expects 1 argument");
        }
        let angle = match parse_angle(arg(args, 0)) {
            Ok(a) => a,
            Err(e) => return e,
        };
        let mut m = Matrix::zero();
        m.set_rotate_y(angle);
        matrix_write(self_, &m);
        none_ref()
    }

    unsafe extern "C" fn set_rotate_z(
        self_: *mut capi::PyObject,
        args: *mut capi::PyObject,
    ) -> *mut capi::PyObject {
        if !want_args(args, 1) {
            return type_error("Matrix.setRotateZ expects 1 argument");
        }
        let angle = match parse_angle(arg(args, 0)) {
            Ok(a) => a,
            Err(e) => return e,
        };
        let mut m = Matrix::zero();
        m.set_rotate_z(angle);
        matrix_write(self_, &m);
        none_ref()
    }

    unsafe extern "C" fn set_rotate_ypr(
        self_: *mut capi::PyObject,
        args: *mut capi::PyObject,
    ) -> *mut capi::PyObject {
        if !want_args(args, 1) {
            return type_error("Matrix.setRotateYPR expects 1 argument");
        }
        let Some(v) = parse_vec3(arg(args, 0)) else {
            return type_error("Matrix.setRotateYPR expects a Vector3");
        };
        // The reference's `VALID_ANGLE_VECTOR3_ARG` checks each element
        // (`script_math.hpp:76-83`).
        for (index, value) in [v.x, v.y, v.z].iter().enumerate() {
            if !(-100.0..=100.0).contains(value) {
                return type_error(&format!(
                    "() argument 1 element {index} must be a valid angle"
                ));
            }
        }
        let mut m = Matrix::zero();
        m.set_rotate_ypr(v.x, v.y, v.z);
        matrix_write(self_, &m);
        none_ref()
    }

    unsafe extern "C" fn pre_multiply(
        self_: *mut capi::PyObject,
        args: *mut capi::PyObject,
    ) -> *mut capi::PyObject {
        if !want_args(args, 1) {
            return type_error("Matrix.preMultiply expects 1 argument");
        }
        let Some(rhs) = provider_matrix(arg(args, 0)) else {
            return type_error("Matrix.preMultiply expects a MatrixProvider");
        };
        let Some(mut m) = matrix_read(self_) else {
            return type_error("not a Matrix");
        };
        m.pre_multiply(&rhs);
        matrix_write(self_, &m);
        none_ref()
    }

    unsafe extern "C" fn post_multiply(
        self_: *mut capi::PyObject,
        args: *mut capi::PyObject,
    ) -> *mut capi::PyObject {
        if !want_args(args, 1) {
            return type_error("Matrix.postMultiply expects 1 argument");
        }
        let Some(rhs) = provider_matrix(arg(args, 0)) else {
            return type_error("Matrix.postMultiply expects a MatrixProvider");
        };
        let Some(mut m) = matrix_read(self_) else {
            return type_error("not a Matrix");
        };
        m.post_multiply(&rhs);
        matrix_write(self_, &m);
        none_ref()
    }

    /// `RETOK` (`script.hpp:1270`): `None` on success, the `ValueError` the
    /// reference's `PyMatrix::invert` raises on a singular matrix.
    unsafe extern "C" fn invert(
        self_: *mut capi::PyObject,
        args: *mut capi::PyObject,
    ) -> *mut capi::PyObject {
        if !want_args(args, 0) {
            return type_error("Matrix.invert takes no arguments");
        }
        let Some(mut m) = matrix_read(self_) else {
            return type_error("not a Matrix");
        };
        if !m.invert() {
            return value_error("Matrix could not be inverted");
        }
        matrix_write(self_, &m);
        none_ref()
    }

    unsafe extern "C" fn look_at(
        self_: *mut capi::PyObject,
        args: *mut capi::PyObject,
    ) -> *mut capi::PyObject {
        if !want_args(args, 3) {
            return type_error("Matrix.lookAt expects 3 arguments");
        }
        let Some(position) = parse_vec3(arg(args, 0)) else {
            return type_error("Matrix.lookAt expects a Vector3 position");
        };
        let Some(direction) = parse_vec3(arg(args, 1)) else {
            return type_error("Matrix.lookAt expects a Vector3 direction");
        };
        let Some(up) = parse_vec3(arg(args, 2)) else {
            return type_error("Matrix.lookAt expects a Vector3 up");
        };
        let mut m = Matrix::zero();
        m.look_at(&position, &direction, &up);
        matrix_write(self_, &m);
        none_ref()
    }

    unsafe extern "C" fn set_element(
        self_: *mut capi::PyObject,
        args: *mut capi::PyObject,
    ) -> *mut capi::PyObject {
        if !want_args(args, 3) {
            return type_error("Matrix.setElement expects 3 arguments");
        }
        let Some(col) = parse_uint(arg(args, 0)) else {
            return type_error("Matrix.setElement expects an integer column");
        };
        let Some(row) = parse_uint(arg(args, 1)) else {
            return type_error("Matrix.setElement expects an integer row");
        };
        let Some(value) = (unsafe { as_float(arg(args, 2)) }) else {
            return type_error("Matrix.setElement expects a float value");
        };
        let Some(mut m) = matrix_read(self_) else {
            return type_error("not a Matrix");
        };
        m.set_element(col, row, value);
        matrix_write(self_, &m);
        none_ref()
    }

    unsafe extern "C" fn get(
        self_: *mut capi::PyObject,
        args: *mut capi::PyObject,
    ) -> *mut capi::PyObject {
        if !want_args(args, 2) {
            return type_error("Matrix.get expects 2 arguments");
        }
        let Some(col) = parse_uint(arg(args, 0)) else {
            return type_error("Matrix.get expects an integer column");
        };
        let Some(row) = parse_uint(arg(args, 1)) else {
            return type_error("Matrix.get expects an integer row");
        };
        let Some(m) = matrix_read(self_) else {
            return type_error("not a Matrix");
        };
        // SAFETY: the interpreter is up (a handler is running).
        unsafe { capi::PyFloat_FromDouble(m.get_element(col, row) as f64) }
    }

    unsafe extern "C" fn apply_point(
        self_: *mut capi::PyObject,
        args: *mut capi::PyObject,
    ) -> *mut capi::PyObject {
        if !want_args(args, 1) {
            return type_error("Matrix.applyPoint expects 1 argument");
        }
        let Some(v) = parse_vec3(arg(args, 0)) else {
            return type_error("Matrix.applyPoint expects a Vector3");
        };
        let Some(m) = matrix_read(self_) else {
            return type_error("not a Matrix");
        };
        let out = m.apply_point(&v);
        super::vector3::make([out.x, out.y, out.z])
    }

    unsafe extern "C" fn apply_v4_point(
        self_: *mut capi::PyObject,
        args: *mut capi::PyObject,
    ) -> *mut capi::PyObject {
        if !want_args(args, 1) {
            return type_error("Matrix.applyV4Point expects 1 argument");
        }
        let Some(v) = parse_vec4(arg(args, 0)) else {
            return type_error("Matrix.applyV4Point expects a Vector4");
        };
        let Some(m) = matrix_read(self_) else {
            return type_error("not a Matrix");
        };
        let out = m.apply_v4_point(&v);
        super::vector4::make([out.x, out.y, out.z, out.w])
    }

    unsafe extern "C" fn apply_vector(
        self_: *mut capi::PyObject,
        args: *mut capi::PyObject,
    ) -> *mut capi::PyObject {
        if !want_args(args, 1) {
            return type_error("Matrix.applyVector expects 1 argument");
        }
        let Some(v) = parse_vec3(arg(args, 0)) else {
            return type_error("Matrix.applyVector expects a Vector3");
        };
        let Some(m) = matrix_read(self_) else {
            return type_error("not a Matrix");
        };
        let out = m.apply_vector(&v);
        super::vector3::make([out.x, out.y, out.z])
    }

    unsafe extern "C" fn apply_to_axis(
        self_: *mut capi::PyObject,
        args: *mut capi::PyObject,
    ) -> *mut capi::PyObject {
        if !want_args(args, 1) {
            return type_error("Matrix.applyToAxis expects 1 argument");
        }
        let Some(axis) = parse_uint(arg(args, 0)) else {
            return type_error("Matrix.applyToAxis expects an integer axis");
        };
        let Some(m) = matrix_read(self_) else {
            return type_error("not a Matrix");
        };
        let out = m.apply_to_unit_axis_vector(axis & 3);
        super::vector3::make([out.x, out.y, out.z])
    }

    unsafe extern "C" fn apply_to_origin(
        self_: *mut capi::PyObject,
        args: *mut capi::PyObject,
    ) -> *mut capi::PyObject {
        if !want_args(args, 0) {
            return type_error("Matrix.applyToOrigin takes no arguments");
        }
        let Some(m) = matrix_read(self_) else {
            return type_error("not a Matrix");
        };
        let out = m.apply_to_origin();
        super::vector3::make([out.x, out.y, out.z])
    }

    unsafe extern "C" fn orthogonal_projection(
        self_: *mut capi::PyObject,
        args: *mut capi::PyObject,
    ) -> *mut capi::PyObject {
        if !want_args(args, 4) {
            return type_error("Matrix.orthogonalProjection expects 4 arguments");
        }
        let mut values = [0.0f32; 4];
        for (index, slot) in values.iter_mut().enumerate() {
            let Some(value) = (unsafe { as_float(arg(args, index)) }) else {
                return type_error("Matrix.orthogonalProjection expects floats");
            };
            *slot = value;
        }
        let mut m = Matrix::zero();
        m.orthogonal_projection(values[0], values[1], values[2], values[3]);
        matrix_write(self_, &m);
        none_ref()
    }

    unsafe extern "C" fn perspective_projection(
        self_: *mut capi::PyObject,
        args: *mut capi::PyObject,
    ) -> *mut capi::PyObject {
        if !want_args(args, 4) {
            return type_error("Matrix.perspectiveProjection expects 4 arguments");
        }
        let mut values = [0.0f32; 4];
        for (index, slot) in values.iter_mut().enumerate() {
            let Some(value) = (unsafe { as_float(arg(args, index)) }) else {
                return type_error("Matrix.perspectiveProjection expects floats");
            };
            *slot = value;
        }
        let mut m = Matrix::zero();
        m.perspective_projection(values[0], values[1], values[2], values[3]);
        matrix_write(self_, &m);
        none_ref()
    }

    // -- `__getstate__`/`__setstate__` (`script_math.cpp:741-772`) ----------

    unsafe extern "C" fn getstate(
        self_: *mut capi::PyObject,
        args: *mut capi::PyObject,
    ) -> *mut capi::PyObject {
        if !want_args(args, 0) {
            return type_error("Matrix.__getstate__ takes no arguments");
        }
        let Some(m) = matrix_read(self_) else {
            return type_error("not a Matrix");
        };
        // SAFETY: the matrix body is 64 bytes of repr(C) f32s, live for the
        // call's duration; `PyBytes_FromStringAndSize` copies them.
        unsafe {
            let ptr = std::ptr::addr_of!(m.m).cast::<c_char>();
            capi::PyBytes_FromStringAndSize(ptr, std::mem::size_of::<Matrix>() as capi::PySsize)
        }
    }

    unsafe extern "C" fn setstate(
        self_: *mut capi::PyObject,
        args: *mut capi::PyObject,
    ) -> *mut capi::PyObject {
        if !want_args(args, 1) {
            return type_error("Matrix.__setstate__ was given wrong args");
        }
        let state = arg(args, 0);
        // SAFETY: the checks read the borrowed object.
        unsafe {
            if !capi::py_bytes_check(state)
                || capi::PyBytes_Size(state) != std::mem::size_of::<Matrix>() as capi::PySsize
            {
                return value_error("Matrix.__setstate__ was given bad state");
            }
            let mut m = [[0.0f32; 4]; 4];
            let src = capi::PyBytes_AsString(state).cast::<u8>();
            std::ptr::copy_nonoverlapping(
                src,
                (&mut m as *mut [[f32; 4]; 4]).cast::<u8>(),
                std::mem::size_of::<Matrix>(),
            );
            matrix_write(self_, &Matrix { m });
        }
        none_ref()
    }

    // -- attribute accessors (`script_math.hpp:198-239`) ---------------------

    unsafe extern "C" fn get_determinant(
        self_: *mut capi::PyObject,
        _closure: *mut std::os::raw::c_void,
    ) -> *mut capi::PyObject {
        let Some(m) = matrix_read(self_) else {
            return type_error("not a Matrix");
        };
        // SAFETY: the interpreter is up.
        unsafe { capi::PyFloat_FromDouble(m.determinant() as f64) }
    }

    unsafe extern "C" fn get_is_mirrored(
        self_: *mut capi::PyObject,
        _closure: *mut std::os::raw::c_void,
    ) -> *mut capi::PyObject {
        let Some(m) = matrix_read(self_) else {
            return type_error("not a Matrix");
        };
        bool_ref(m.is_mirrored())
    }

    unsafe extern "C" fn get_translation(
        self_: *mut capi::PyObject,
        _closure: *mut std::os::raw::c_void,
    ) -> *mut capi::PyObject {
        let Some(m) = matrix_read(self_) else {
            return type_error("not a Matrix");
        };
        let out = m.apply_to_origin();
        super::vector3::make([out.x, out.y, out.z])
    }

    unsafe extern "C" fn set_translation(
        self_: *mut capi::PyObject,
        value: *mut capi::PyObject,
        _closure: *mut std::os::raw::c_void,
    ) -> c_int {
        let Some(v) = parse_vec3(value) else {
            let _ = type_error("translation must be a Vector3");
            return -1;
        };
        let Some(mut m) = matrix_read(self_) else {
            let _ = type_error("not a Matrix");
            return -1;
        };
        m.translation(&v);
        matrix_write(self_, &m);
        0
    }

    unsafe extern "C" fn get_yaw(
        self_: *mut capi::PyObject,
        _closure: *mut std::os::raw::c_void,
    ) -> *mut capi::PyObject {
        let Some(m) = matrix_read(self_) else {
            return type_error("not a Matrix");
        };
        // SAFETY: the interpreter is up.
        unsafe { capi::PyFloat_FromDouble(m.yaw() as f64) }
    }

    unsafe extern "C" fn get_pitch(
        self_: *mut capi::PyObject,
        _closure: *mut std::os::raw::c_void,
    ) -> *mut capi::PyObject {
        let Some(m) = matrix_read(self_) else {
            return type_error("not a Matrix");
        };
        // SAFETY: the interpreter is up.
        unsafe { capi::PyFloat_FromDouble(m.pitch() as f64) }
    }

    unsafe extern "C" fn get_roll(
        self_: *mut capi::PyObject,
        _closure: *mut std::os::raw::c_void,
    ) -> *mut capi::PyObject {
        let Some(m) = matrix_read(self_) else {
            return type_error("not a Matrix");
        };
        // SAFETY: the interpreter is up.
        unsafe { capi::PyFloat_FromDouble(m.roll() as f64) }
    }

    // -- the `Math.Matrix(...)` factory (`script_math.cpp:781-796`) ---------

    /// `Math.Matrix`: a fresh matrix, optionally copied from a
    /// `MatrixProvider`. The factory is a plain function, not the type, the
    /// reference's `PY_FACTORY_NAMED`.
    unsafe extern "C" fn factory(
        _module: *mut capi::PyObject,
        args: *mut capi::PyObject,
    ) -> *mut capi::PyObject {
        let count = unsafe { capi::PyTuple_Size(args) };
        let m = match count {
            0 => Matrix::identity(),
            1 => match provider_matrix(arg(args, 0)) {
                Some(m) => m,
                None => return type_error("Math.Matrix(): Expected an optional MatrixProvider"),
            },
            _ => return type_error("Math.Matrix(): Expected an optional MatrixProvider"),
        };
        make(&m)
    }

    /// The factory descriptor for `PyCFunction_New`; leaked for the type's
    /// lifetime, the reference's static method table.
    fn factory_def() -> *mut capi::PyMethodDef {
        Box::leak(Box::new(capi::PyMethodDef {
            ml_name: c"Matrix".as_ptr() as *const c_char,
            ml_meth: Some(factory),
            ml_flags: capi::METH_VARARGS,
            ml_doc: std::ptr::null(),
        }))
    }

    /// `Math.MatrixProvider`'s spec: the abstract base, no methods or
    /// attributes, and the reference's refusal to construct it directly.
    pub(super) fn provider_spec() -> binding::TypeSpec {
        binding::TypeSpec::new("Math.MatrixProvider").flags(capi::Py_TPFLAGS_BASETYPE)
    }

    /// The `PY_BEGIN_METHODS` table and `PY_BEGIN_ATTRIBUTES` table of
    /// `PyMatrix` (`script_math.cpp:...`), the port's builder shape of the
    /// reference's macro pair.
    pub(super) fn spec() -> binding::TypeSpec {
        binding::TypeSpec::new("Math.Matrix")
            .base(provider_addr())
            .attribute("determinant", get_determinant, None)
            .attribute("isMirrored", get_is_mirrored, None)
            .attribute("translation", get_translation, Some(set_translation))
            .attribute("yaw", get_yaw, None)
            .attribute("pitch", get_pitch, None)
            .attribute("roll", get_roll, None)
            .method("set", set)
            .method("setZero", set_zero)
            .method("setIdentity", set_identity)
            .method("setScale", set_scale)
            .method("setTranslate", set_translate)
            .method("setRotateX", set_rotate_x)
            .method("setRotateY", set_rotate_y)
            .method("setRotateZ", set_rotate_z)
            .method("setRotateYPR", set_rotate_ypr)
            .method("preMultiply", pre_multiply)
            .method("postMultiply", post_multiply)
            .method("invert", invert)
            .method("lookAt", look_at)
            .method("setElement", set_element)
            .method("get", get)
            .method("applyPoint", apply_point)
            .method("applyV4Point", apply_v4_point)
            .method("applyVector", apply_vector)
            .method("applyToAxis", apply_to_axis)
            .method("applyToOrigin", apply_to_origin)
            .method("orthogonalProjection", orthogonal_projection)
            .method("perspectiveProjection", perspective_projection)
            .method("__getstate__", getstate)
            .method("__setstate__", setstate)
            .instance_size(INSTANCE_SIZE)
    }

    /// `TempPyVectorInit` for the matrix pair: the `MatrixProvider` base, the
    /// `Matrix` type deriving from it, the `MatrixProvider` name and the
    /// `Matrix` factory into the `Math` module dictionary.
    pub(super) fn register() -> Result<PyRef, binding::BindError> {
        // The base must exist before the derived type resolves its
        // `Py_tp_base` slot.
        let provider = binding::create_type(&provider_spec())
            .inspect_err(|e| eprintln!("matrix provider create_type failed: {e:?}"))?;
        PROVIDER_ADDR.store(provider.as_ptr() as usize, Ordering::SeqCst);
        let matrix_type = binding::create_type(&spec())
            .inspect_err(|e| eprintln!("matrix create_type failed: {e:?}"))?;
        TYPE_ADDR.store(matrix_type.as_ptr() as usize, Ordering::SeqCst);

        // SAFETY: `register` runs from interpreter init; the interpreter is up.
        unsafe {
            let module = capi::PyImport_AddModule(c"Math".as_ptr());
            if !module.is_null() {
                let dict = capi::PyModule_GetDict(module);
                capi::PyDict_SetItemString(dict, c"MatrixProvider".as_ptr(), provider.as_ptr());
                let factory = capi::PyCFunction_New(factory_def(), std::ptr::null_mut());
                if !factory.is_null() {
                    capi::PyDict_SetItemString(dict, c"Matrix".as_ptr(), factory);
                    capi::dec_ref(factory);
                }
            }
        }
        Ok(matrix_type)
    }
}

/// The `Math.Plane` type (`script_math.cpp:5227-5292`): `PyPlane` is a
/// `PyObjectPlus` carrying a `PlaneEq`, exposing only `init` and
/// `intersectRay` the way the reference's `PY_BEGIN_METHODS` table does.
mod plane {
    use super::*;
    use foundation::math::PlaneEq;

    /// The instance body beyond the `PyObject` header: the plane equation
    /// inline, the layout of the reference's `PyPlane` (which embeds a
    /// `PlaneEq`).
    #[repr(C)]
    struct PlaneBody {
        header: capi::PyObject,
        plane: PlaneEq,
    }

    const INSTANCE_SIZE: usize =
        std::mem::size_of::<PlaneBody>() - std::mem::size_of::<capi::PyObject>();

    /// The one `Math.Plane` type object, by address.
    static TYPE_ADDR: AtomicUsize = AtomicUsize::new(0);

    fn type_addr() -> usize {
        TYPE_ADDR.load(Ordering::SeqCst)
    }

    fn plane_read(object: *mut capi::PyObject) -> Option<PlaneEq> {
        if object.is_null() {
            return None;
        }
        // SAFETY: the type check gates the body read to `PlaneBody`.
        unsafe {
            if capi::Py_TYPE(object) as usize != type_addr() {
                return None;
            }
            Some(std::ptr::addr_of!((*object.cast::<PlaneBody>()).plane).read())
        }
    }

    fn plane_write(object: *mut capi::PyObject, plane: &PlaneEq) -> bool {
        if object.is_null() {
            return false;
        }
        // SAFETY: the type check gates the body write to `PlaneBody`.
        unsafe {
            if capi::Py_TYPE(object) as usize != type_addr() {
                return false;
            }
            std::ptr::addr_of_mut!((*object.cast::<PlaneBody>()).plane).write(*plane);
        }
        true
    }

    /// A fresh `Math.Plane` carrying `plane`; the caller owns the reference.
    fn make(plane: &PlaneEq) -> *mut capi::PyObject {
        let Ok(type_object) = binding::create_type(&spec()) else {
            return std::ptr::null_mut();
        };
        let Some(instance) = (unsafe { binding::instantiate(&type_object) }) else {
            return std::ptr::null_mut();
        };
        let pointer = instance.as_ptr();
        // SAFETY: the allocated body is exactly a `PlaneBody`.
        unsafe {
            std::ptr::addr_of_mut!((*pointer.cast::<PlaneBody>()).plane).write(*plane);
        }
        std::mem::forget(instance);
        pointer
    }

    /// `PyPlane::py_init` (`script_math.cpp`): the plane through the three
    /// points, the reference's `ARG(Vector3, ARG(Vector3, ARG(Vector3, END)))`.
    unsafe extern "C" fn init(
        self_: *mut capi::PyObject,
        args: *mut capi::PyObject,
    ) -> *mut capi::PyObject {
        if !want_args(args, 3) {
            return type_error("Plane.init expects 3 Vector3 arguments");
        }
        let Some(p0) = parse_vec3(arg(args, 0)) else {
            return type_error("Plane.init expects 3 Vector3 arguments");
        };
        let Some(p1) = parse_vec3(arg(args, 1)) else {
            return type_error("Plane.init expects 3 Vector3 arguments");
        };
        let Some(p2) = parse_vec3(arg(args, 2)) else {
            return type_error("Plane.init expects 3 Vector3 arguments");
        };
        let Some(mut plane) = plane_read(self_) else {
            return type_error("Plane.init on a non-Plane");
        };
        plane.init(&p0, &p1, &p2);
        plane_write(self_, &plane);
        none_ref()
    }

    /// `PyPlane::py_intersectRay` (`script_math.cpp`): the hit point of a ray
    /// from `src` along `dst`.
    unsafe extern "C" fn intersect_ray(
        self_: *mut capi::PyObject,
        args: *mut capi::PyObject,
    ) -> *mut capi::PyObject {
        if !want_args(args, 2) {
            return type_error("Plane.intersectRay expects 2 Vector3 arguments");
        }
        let Some(source) = parse_vec3(arg(args, 0)) else {
            return type_error("Plane.intersectRay expects 2 Vector3 arguments");
        };
        let Some(dir) = parse_vec3(arg(args, 1)) else {
            return type_error("Plane.intersectRay expects 2 Vector3 arguments");
        };
        let Some(plane) = plane_read(self_) else {
            return type_error("Plane.intersectRay on a non-Plane");
        };
        let hit = plane.intersect_ray(&source, &dir);
        super::vector3::make([hit.x, hit.y, hit.z])
    }

    /// `PyPlane::pyNew` (`script_math.cpp`): the factory ignores its
    /// arguments, the reference's `PY_FACTORY_NAMED( PyPlane, "Plane", Math )`.
    unsafe extern "C" fn factory(
        _module: *mut capi::PyObject,
        _args: *mut capi::PyObject,
    ) -> *mut capi::PyObject {
        make(&PlaneEq::default())
    }

    /// The factory descriptor for `PyCFunction_New`; leaked for the type's
    /// lifetime, the reference's static method table.
    fn factory_def() -> *mut capi::PyMethodDef {
        Box::leak(Box::new(capi::PyMethodDef {
            ml_name: c"Plane".as_ptr() as *const c_char,
            ml_meth: Some(factory),
            ml_flags: capi::METH_VARARGS,
            ml_doc: std::ptr::null(),
        }))
    }

    /// The `PY_BEGIN_METHODS` table of `PyPlane` (`script_math.cpp:...`).
    fn spec() -> binding::TypeSpec {
        binding::TypeSpec::new("Math.Plane")
            .method("init", init)
            .method("intersectRay", intersect_ray)
            .instance_size(INSTANCE_SIZE)
    }

    /// `TempPyVectorInit` for the plane: the type object and its factory into
    /// the `Math` module dictionary.
    pub(super) fn register() -> Result<PyRef, binding::BindError> {
        let type_object = binding::create_type(&spec())?;
        TYPE_ADDR.store(type_object.as_ptr() as usize, Ordering::SeqCst);

        // SAFETY: `register` runs from interpreter init; the interpreter is up.
        unsafe {
            let module = capi::PyImport_AddModule(c"Math".as_ptr());
            if !module.is_null() {
                let dict = capi::PyModule_GetDict(module);
                let factory = capi::PyCFunction_New(factory_def(), std::ptr::null_mut());
                if !factory.is_null() {
                    capi::PyDict_SetItemString(dict, c"Plane".as_ptr(), factory);
                    capi::dec_ref(factory);
                }
            }
        }
        Ok(type_object)
    }
}

/// One provider type's destruction entry: release every held field, then
/// hand the allocation back to the interpreter's allocator. The reference's
/// `SmartPointer` members are Python references, so a provider whose body
/// holds other providers must release them before the object is freed.
macro_rules! provider_dealloc {
    ($name:ident, $body:ty, $($field:ident),+ $(,)?) => {
        unsafe extern "C" fn $name(self_: *mut capi::PyObject) {
            $(
                unsafe {
                    v4provider::release_field::<{ std::mem::offset_of!($body, $field) }>(self_);
                }
            )+
            unsafe { capi::PyObject_Free(self_.cast()) };
        }
    };
}

/// The `Math.Vector4Provider` base and `Math.Vector4Basic` value
/// (`script_math.cpp:3150-3282`).
///
/// `Vector4Provider` is the abstract base every script-side `Vector4` source
/// derives from; its one member is the read-only `value` attribute the
/// reference's `pyGet_value` fills from the virtual `output`
/// (`:3190-3195`). The reference's vtable is the port's [`OUTPUT`] registry:
/// each concrete provider records its `output` at registration, and
/// [`output_of`] looks it up by the object's concrete type address.
///
/// `Vector4Basic` is the constant provider `coerce` produces when a script
/// hands a provider slot a `Vector4` or a tuple of four floats
/// (`Vector4Provider::coerce`, `:3266-3279`); the reference gives it no
/// factory and no `Math` name, so neither is registered here.
mod v4provider {
    use super::*;
    use std::collections::BTreeMap;
    use std::sync::Mutex;

    /// The one `Math.Vector4Provider` base type object, by address.
    static BASE_ADDR: AtomicUsize = AtomicUsize::new(0);
    /// The one `Math.Vector4Basic` type object, by address.
    static BASIC_ADDR: AtomicUsize = AtomicUsize::new(0);

    /// The base type object address, the concrete providers' `Py_tp_base`.
    pub(super) fn base_addr() -> usize {
        BASE_ADDR.load(Ordering::SeqCst)
    }

    /// A concrete provider's `output`: the four floats it yields for its
    /// instance.
    type Output = fn(*mut capi::PyObject) -> [f32; 4];

    /// The concrete providers' `output`s, keyed by type object address: the
    /// reference's virtual `Vector4Provider::output` dispatch.
    static OUTPUT: Mutex<BTreeMap<usize, Output>> = Mutex::new(BTreeMap::new());

    /// `Vector4Provider::Check` (`script.hpp:585`): any object whose type is
    /// `Vector4Provider` or one of its subclasses. The keyframe converter
    /// calls it on a coerced candidate, so it leaves the module.
    pub(super) fn provider_check(object: *mut capi::PyObject) -> bool {
        let base = BASE_ADDR.load(Ordering::SeqCst);
        if object.is_null() || base == 0 {
            return false;
        }
        // SAFETY: the address is the type object `create_type` built.
        unsafe {
            capi::PyType_IsSubtype(capi::Py_TYPE(object), base as *mut capi::PyTypeObject) == 1
        }
    }

    /// Record one concrete provider's `output` under its type object address.
    /// Called from each provider's `register`, before a script can run.
    pub(super) fn register_output(type_addr: usize, output: Output) {
        OUTPUT
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(type_addr, output);
    }

    /// The output of a held provider, the reference's virtual `output`. `None`
    /// when the object is not a `Vector4Provider` or no `output` is registered
    /// for its type.
    pub(super) fn output_of(object: *mut capi::PyObject) -> Option<[f32; 4]> {
        if !provider_check(object) {
            return None;
        }
        let output = {
            let table = OUTPUT
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            table
                .get(&unsafe { capi::Py_TYPE(object) as usize })
                .copied()
        };
        output.map(|output| output(object))
    }

    // -- the `Vector4ProviderPtr` slot family, shared by the providers that
    //    hold one (`Vector4MatrixAdaptor`'s source and position, the
    //    swizzle's four slots) --

    /// The stored provider of a slot, by byte offset from the object start.
    ///
    /// # Safety
    ///
    /// `self_` is a borrowed instance of the slot's owner type, and `FIELD`
    /// is that body's offset of the slot.
    pub(super) unsafe fn slot_read<const FIELD: usize>(
        self_: *mut capi::PyObject,
    ) -> *mut capi::PyObject {
        unsafe {
            std::ptr::addr_of!(*self_.cast::<u8>().add(FIELD).cast::<*mut capi::PyObject>()).read()
        }
    }

    /// Store `value` in the slot. The caller releases the previous occupant
    /// first, the reference's `SmartPointer` assignment half.
    ///
    /// # Safety
    ///
    /// As [`slot_read`].
    pub(super) unsafe fn slot_write<const FIELD: usize>(
        self_: *mut capi::PyObject,
        value: *mut capi::PyObject,
    ) {
        unsafe {
            std::ptr::addr_of_mut!(*self_.cast::<u8>().add(FIELD).cast::<*mut capi::PyObject>())
                .write(value)
        }
    }

    /// `Script::getData( Vector4ProviderPtr )` (`script.hpp:604-608`) on a
    /// held slot: the stored provider, or `None` for an empty slot.
    ///
    /// # Safety
    ///
    /// `self_` is a borrowed instance of the slot's owner type.
    pub(super) unsafe extern "C" fn slot_get<const FIELD: usize>(
        self_: *mut capi::PyObject,
        _closure: *mut std::os::raw::c_void,
    ) -> *mut capi::PyObject {
        let stored = unsafe { slot_read::<FIELD>(self_) };
        let object = if stored.is_null() {
            capi::py_none()
        } else {
            stored
        };
        // SAFETY: the slot's occupant is a borrowed reference the read does
        // not consume; the caller gets its own.
        unsafe {
            capi::inc_ref(object);
        }
        object
    }

    /// `Script::setData( pObject, Vector4ProviderPtr&, varName )`
    /// (`script.hpp:610-631`): the `Vector4Provider::coerce` runs first
    /// (`:3266-3279`), so a `Vector4` or a tuple of four floats becomes a
    /// fresh `Vector4Basic`; a `None` empties the slot; anything the
    /// coercion left that is not a provider is refused with the converter's
    /// own error. The field owns one reference the way the reference's
    /// `SmartPointer` does, the previous occupant released before the new
    /// one is stored, and a self-assignment a no-op.
    ///
    /// # Safety
    ///
    /// As [`slot_get`].
    pub(super) unsafe extern "C" fn slot_set<const FIELD: usize>(
        self_: *mut capi::PyObject,
        value: *mut capi::PyObject,
        closure: *mut std::os::raw::c_void,
    ) -> c_int {
        if value.is_null() {
            type_error(&format!(
                "{} must be set to a Vector4Provider or None",
                attribute_name(closure)
            ));
            return -1;
        }
        // `PyObjectPtr pCoerced = CLASS::coerce( pObject )`: a reference this
        // body either owns outright (a fresh `Vector4Basic`) or holds one
        // added reference of.
        let coerced = coerce(value);
        if coerced == capi::py_none() {
            // `pCoerced == Py_None`: the slot is emptied.
            unsafe { capi::dec_ref(coerced) };
            let old = unsafe { slot_read::<FIELD>(self_) };
            if !old.is_null() {
                unsafe { capi::dec_ref(old) };
            }
            unsafe { slot_write::<FIELD>(self_, std::ptr::null_mut()) };
            return 0;
        }
        if !provider_check(coerced) {
            unsafe { capi::dec_ref(coerced) };
            type_error(&format!(
                "{} must be set to a Vector4Provider or None",
                attribute_name(closure)
            ));
            return -1;
        }
        let old = unsafe { slot_read::<FIELD>(self_) };
        if old != coerced {
            if !old.is_null() {
                unsafe { capi::dec_ref(old) };
            }
            // The slot takes over the coerced reference, the assignment's
            // `SmartPointer` incref half already paid by `coerce`.
            unsafe { slot_write::<FIELD>(self_, coerced) };
        } else {
            // The self-assignment no-op: the coercion's reference goes back.
            unsafe { capi::dec_ref(coerced) };
        }
        0
    }

    // -- `Math.Vector4Basic` ---------------------------------------------

    /// The instance body beyond the `PyObject` header: the four f32s inline,
    /// the layout of the reference's `Vector4Basic` (which embeds a
    /// `Vector4`).
    #[repr(C)]
    struct BasicBody {
        header: capi::PyObject,
        data: [f32; 4],
    }

    const BASIC_INSTANCE_SIZE: usize =
        std::mem::size_of::<BasicBody>() - std::mem::size_of::<capi::PyObject>();

    /// `Vector4Basic::output` (`script_math.hpp:494`): the stored value.
    fn basic_output(object: *mut capi::PyObject) -> [f32; 4] {
        // SAFETY: only a `Vector4Basic` instance reaches this arm.
        unsafe { std::ptr::addr_of!((*object.cast::<BasicBody>()).data).read() }
    }

    /// A fresh `Math.Vector4Basic` carrying `data`; the caller owns the
    /// reference.
    pub(super) fn basic_make(data: [f32; 4]) -> *mut capi::PyObject {
        let Ok(type_object) = binding::create_type(&basic_spec()) else {
            return std::ptr::null_mut();
        };
        let Some(instance) = (unsafe { binding::instantiate(&type_object) }) else {
            return std::ptr::null_mut();
        };
        let pointer = instance.as_ptr();
        // SAFETY: the allocated body is exactly a `BasicBody`.
        unsafe {
            std::ptr::addr_of_mut!((*pointer.cast::<BasicBody>()).data).write(data);
        }
        std::mem::forget(instance);
        pointer
    }

    /// `Vector4Provider::coerce` (`:3266-3279`): a `Vector4` or a tuple of
    /// four floats becomes a fresh `Vector4Basic`; anything else is handed
    /// back unchanged, the reference clearing the conversion error first.
    /// The caller owns the returned reference: the one the fresh instance
    /// carries, or an added reference on the returned object.
    pub(super) fn coerce(object: *mut capi::PyObject) -> *mut capi::PyObject {
        if let Some(data) = unsafe { as_vector::<4>(object, vector4::type_addr()) } {
            return basic_make(data);
        }
        // SAFETY: the read is on the borrowed object; a failure leaves no
        // pending error (`as_vector` clears it).
        unsafe {
            if object.is_null() {
                return std::ptr::null_mut();
            }
            capi::inc_ref(object);
        }
        object
    }

    // -- the base's read-only `value` ------------------------------------

    /// `Vector4Provider::pyGet_value`: the provider's output as a `Vector4`.
    unsafe extern "C" fn get_value(
        self_: *mut capi::PyObject,
        _closure: *mut std::os::raw::c_void,
    ) -> *mut capi::PyObject {
        match output_of(self_) {
            Some(data) => vector4::make(data),
            None => type_error("Vector4Provider.value on a non-Vector4Provider"),
        }
    }

    // -- `Vector4Basic`'s read-write `value` -----------------------------

    /// `Vector4Basic`'s `value` read (`PY_RW_ATTRIBUTE_DECLARE( value_, value )`):
    /// the stored vector.
    unsafe extern "C" fn basic_get_value(
        self_: *mut capi::PyObject,
        _closure: *mut std::os::raw::c_void,
    ) -> *mut capi::PyObject {
        if unsafe { capi::Py_TYPE(self_) } as usize != BASIC_ADDR.load(Ordering::SeqCst) {
            return type_error("Vector4Basic.value on a non-Vector4Basic");
        }
        let data = unsafe { std::ptr::addr_of!((*self_.cast::<BasicBody>()).data).read() };
        vector4::make(data)
    }

    /// `Vector4Basic`'s `value` write: `Script::setData(value, value_)`
    /// (`script.cpp:1946-1970`) -- a `Vector4` or a tuple of four floats.
    unsafe extern "C" fn basic_set_value(
        self_: *mut capi::PyObject,
        value: *mut capi::PyObject,
        _closure: *mut std::os::raw::c_void,
    ) -> c_int {
        if unsafe { capi::Py_TYPE(self_) } as usize != BASIC_ADDR.load(Ordering::SeqCst) {
            type_error(&format!(
                "{} must be set to a Vector4 or tuple of 4 floats",
                attribute_name(_closure)
            ));
            return -1;
        }
        let Some(data) = (unsafe { as_vector::<4>(value, vector4::type_addr()) }) else {
            type_error(&format!(
                "{} must be set to a Vector4 or tuple of 4 floats",
                attribute_name(_closure)
            ));
            return -1;
        };
        // SAFETY: the type check above gated the write to `BasicBody`.
        unsafe {
            std::ptr::addr_of_mut!((*self_.cast::<BasicBody>()).data).write(data);
        }
        0
    }

    // -- the held-provider attribute family, shared by every provider -----

    /// The field's stored provider, by byte offset from the object start.
    ///
    /// # Safety
    ///
    /// `self_` is a borrowed instance whose body is the field's owner, and
    /// `FIELD` is that body's offset of the provider slot.
    pub(super) unsafe fn provider_field<const FIELD: usize>(
        self_: *mut capi::PyObject,
    ) -> *mut capi::PyObject {
        unsafe {
            std::ptr::addr_of!(*self_.cast::<u8>().add(FIELD).cast::<*mut capi::PyObject>()).read()
        }
    }

    /// Store `value` in the field. The caller releases the previous occupant
    /// first, the reference's `SmartPointer` assignment half.
    ///
    /// # Safety
    ///
    /// As [`provider_field`]: `FIELD` is the field's offset in `self_`'s body.
    pub(super) unsafe fn provider_write<const FIELD: usize>(
        self_: *mut capi::PyObject,
        value: *mut capi::PyObject,
    ) {
        unsafe {
            std::ptr::addr_of_mut!(*self_.cast::<u8>().add(FIELD).cast::<*mut capi::PyObject>())
                .write(value)
        }
    }

    /// Release the field's occupant, the reference's `SmartPointer`
    /// destructor half.
    ///
    /// # Safety
    ///
    /// As [`provider_field`].
    pub(super) unsafe fn release_field<const FIELD: usize>(self_: *mut capi::PyObject) {
        let stored = unsafe { provider_field::<FIELD>(self_) };
        if !stored.is_null() {
            unsafe { capi::dec_ref(stored) };
        }
    }

    /// `Script::getData( ScriptObjectPtr )` (`script.hpp:604-608`) on a held
    /// field: the stored provider, or `None` for an empty field.
    ///
    /// # Safety
    ///
    /// `self_` is a borrowed instance of the field's owner type.
    pub(super) unsafe extern "C" fn provider_ptr_get<const FIELD: usize>(
        self_: *mut capi::PyObject,
        _closure: *mut std::os::raw::c_void,
    ) -> *mut capi::PyObject {
        let stored = unsafe { provider_field::<FIELD>(self_) };
        let object = if stored.is_null() {
            capi::py_none()
        } else {
            stored
        };
        // SAFETY: the field's occupant is a borrowed reference the read does
        // not consume; the caller gets its own.
        unsafe {
            capi::inc_ref(object);
        }
        object
    }

    /// `Script::setData( pObject, Vector4ProviderPtr&, varName )`
    /// (`script.hpp:577-631`): `coerce` the value, empty the field for
    /// `None`, and otherwise hand the field the coerced provider. The field
    /// owns the reference the way the reference's `SmartPointer` does: the
    /// previous occupant is released before the new one is stored, and a
    /// self-assignment is a no-op.
    ///
    /// # Safety
    ///
    /// As [`provider_ptr_get`].
    pub(super) unsafe extern "C" fn provider_ptr_set<const FIELD: usize>(
        self_: *mut capi::PyObject,
        value: *mut capi::PyObject,
        closure: *mut std::os::raw::c_void,
    ) -> c_int {
        let coerced = coerce(value);
        if !coerced.is_null() && coerced == capi::py_none() {
            // `pCoerced == Py_None`: the field is emptied and the reference
            // `coerce` added on `None` goes back.
            let old = unsafe { provider_field::<FIELD>(self_) };
            if !old.is_null() {
                unsafe { capi::dec_ref(old) };
            }
            unsafe { provider_write::<FIELD>(self_, std::ptr::null_mut()) };
            unsafe { capi::dec_ref(coerced) };
            return 0;
        }
        if !provider_check(coerced) {
            if !coerced.is_null() {
                unsafe { capi::dec_ref(coerced) };
            }
            type_error(&format!(
                "{} must be set to a Vector4Provider or None",
                attribute_name(closure)
            ));
            return -1;
        }
        let old = unsafe { provider_field::<FIELD>(self_) };
        if old != coerced {
            if !old.is_null() {
                unsafe { capi::dec_ref(old) };
            }
            // The field takes over the reference `coerce` added.
            unsafe { provider_write::<FIELD>(self_, coerced) };
        } else {
            // `rpDerived.get() == pCoerced`: the reference no-ops, so the
            // added reference goes back and the field keeps its own.
            unsafe { capi::dec_ref(coerced) };
        }
        0
    }

    // -- the specs and registration ---------------------------------------

    /// `Math.Vector4Provider`'s spec: the abstract base. It is not named in
    /// the `Math` module, the reference's `PY_TYPEOBJECT` with no factory
    /// link.
    fn provider_spec() -> binding::TypeSpec {
        binding::TypeSpec::new("Math.Vector4Provider")
            .flags(capi::Py_TPFLAGS_BASETYPE)
            .attribute("value", get_value, None)
    }

    /// `Vector4Basic`'s spec: the read-write `value` that shadows the base's
    /// read-only one.
    fn basic_spec() -> binding::TypeSpec {
        binding::TypeSpec::new("Math.Vector4Basic")
            .base(BASE_ADDR.load(Ordering::SeqCst))
            .attribute("value", basic_get_value, Some(basic_set_value))
            .instance_size(BASIC_INSTANCE_SIZE)
    }

    /// The `TempPyVectorInit` of the provider pair: the base, then the
    /// concrete value type and its dispatch entry. Neither is named in the
    /// `Math` module, the reference's `PY_TYPEOBJECT` shape with no
    /// `PY_FACTORY`.
    pub(super) fn register() -> Result<PyRef, binding::BindError> {
        let provider = binding::create_type(&provider_spec())?;
        BASE_ADDR.store(provider.as_ptr() as usize, Ordering::SeqCst);
        let basic = binding::create_type(&basic_spec())?;
        BASIC_ADDR.store(basic.as_ptr() as usize, Ordering::SeqCst);
        register_output(BASIC_ADDR.load(Ordering::SeqCst), basic_output);
        Ok(basic)
    }
}

/// The `Math.Vector4Product` provider (`script_math.cpp:3289-3421`): the
/// componentwise product of the outputs of two held `Vector4Provider`s.
///
/// The reference's `output` reads `a` then `b` in that order and multiplies
/// component by component (`Vector4::parallelMultiply`), falling back to the
/// one source that is set and to the zero vector when neither is
/// (`:3401-3421`). The port keeps the read order and the fallbacks, the
/// evaluation-order acceptance.
mod v4product {
    use super::*;

    /// The instance body beyond the `PyObject` header: the two held provider
    /// references, the layout of the reference's `Vector4Product`.
    #[repr(C)]
    struct ProductBody {
        header: capi::PyObject,
        a: *mut capi::PyObject,
        b: *mut capi::PyObject,
    }

    const A: usize = std::mem::offset_of!(ProductBody, a);
    const B: usize = std::mem::offset_of!(ProductBody, b);

    const INSTANCE_SIZE: usize =
        std::mem::size_of::<ProductBody>() - std::mem::size_of::<capi::PyObject>();

    /// The one `Math.Vector4Product` type object, by address.
    static TYPE_ADDR: AtomicUsize = AtomicUsize::new(0);

    fn type_addr() -> usize {
        TYPE_ADDR.load(Ordering::SeqCst)
    }

    /// `Vector4Product::output` (`script_math.cpp:3401-3421`).
    fn product_output(object: *mut capi::PyObject) -> [f32; 4] {
        let a = unsafe { v4provider::provider_field::<A>(object) };
        let b = unsafe { v4provider::provider_field::<B>(object) };
        if !a.is_null() {
            let mut out = v4provider::output_of(a).unwrap_or([0.0; 4]);
            if !b.is_null() {
                let other = v4provider::output_of(b).unwrap_or([0.0; 4]);
                for (slot, factor) in out.iter_mut().zip(other) {
                    *slot *= factor;
                }
            }
            out
        } else if !b.is_null() {
            v4provider::output_of(b).unwrap_or([0.0; 4])
        } else {
            [0.0; 4]
        }
    }

    /// A fresh `Math.Vector4Product`; the caller owns the reference.
    fn make() -> *mut capi::PyObject {
        let Ok(type_object) = binding::create_type(&spec()) else {
            return std::ptr::null_mut();
        };
        let Some(instance) = (unsafe { binding::instantiate(&type_object) }) else {
            return std::ptr::null_mut();
        };
        let pointer = instance.as_ptr();
        // SAFETY: the allocated body is exactly a `ProductBody`; the two
        // fields start empty, `PyType_GenericAlloc` zeroes the body.
        unsafe {
            std::ptr::addr_of_mut!((*pointer.cast::<ProductBody>()).a).write(std::ptr::null_mut());
            std::ptr::addr_of_mut!((*pointer.cast::<ProductBody>()).b).write(std::ptr::null_mut());
        }
        std::mem::forget(instance);
        pointer
    }

    /// The `_pyNew`-shape factory of `PY_FACTORY( Vector4Product, Math )`:
    /// a fresh product, the reference's `END` argument list meaning no
    /// arguments.
    unsafe extern "C" fn factory(
        _module: *mut capi::PyObject,
        args: *mut capi::PyObject,
    ) -> *mut capi::PyObject {
        if !want_args(args, 0) {
            return type_error("Math.Vector4Product() takes no arguments");
        }
        make()
    }

    /// The factory descriptor for `PyCFunction_New`; leaked for the type's
    /// lifetime, the reference's static method table.
    fn factory_def() -> *mut capi::PyMethodDef {
        Box::leak(Box::new(capi::PyMethodDef {
            ml_name: c"Vector4Product".as_ptr() as *const c_char,
            ml_meth: Some(factory),
            ml_flags: capi::METH_VARARGS,
            ml_doc: std::ptr::null(),
        }))
    }

    /// The `PY_BEGIN_ATTRIBUTES` table of `Vector4Product` and its
    /// destruction entry: the two held providers, each the reference's
    /// `PY_RW_ATTRIBUTE_DECLARE( a_, a )` converter pair.
    fn spec() -> binding::TypeSpec {
        binding::TypeSpec::new("Math.Vector4Product")
            .base(v4provider::base_addr())
            .attribute_with(
                "a",
                v4provider::provider_ptr_get::<A>,
                Some(v4provider::provider_ptr_set::<A>),
                c"a".as_ptr() as *mut std::os::raw::c_void,
            )
            .attribute_with(
                "b",
                v4provider::provider_ptr_get::<B>,
                Some(v4provider::provider_ptr_set::<B>),
                c"b".as_ptr() as *mut std::os::raw::c_void,
            )
            .dealloc(dealloc)
            .instance_size(INSTANCE_SIZE)
    }

    provider_dealloc!(dealloc, ProductBody, a, b);

    /// `TempPyVectorInit` for the product: the type object, its dispatch
    /// entry, and the factory into the `Math` module dictionary.
    pub(super) fn register() -> Result<PyRef, binding::BindError> {
        let type_object = binding::create_type(&spec())?;
        TYPE_ADDR.store(type_object.as_ptr() as usize, Ordering::SeqCst);
        v4provider::register_output(type_addr(), product_output);

        // SAFETY: `register` runs from interpreter init; the interpreter is up.
        unsafe {
            let module = capi::PyImport_AddModule(c"Math".as_ptr());
            if !module.is_null() {
                let dict = capi::PyModule_GetDict(module);
                let factory = capi::PyCFunction_New(factory_def(), std::ptr::null_mut());
                if !factory.is_null() {
                    capi::PyDict_SetItemString(dict, c"Vector4Product".as_ptr(), factory);
                    capi::dec_ref(factory);
                }
            }
        }
        Ok(type_object)
    }
}

/// The `Math.Vector4Animation` provider (`script_math.cpp:3425-3673`): the
/// keyframed animation between `Vector4Provider`s.
///
/// The reference's `output` holds the first keyframe before the cycle
/// reaches it, the last after it passes, and lerps between the two
/// neighbours otherwise (`:3640-3671`); the port walks the same list once,
/// reads the `oit` output before the `nit` output, and lerps with the
/// reference's `XPVec4Lerp`. The `tick` advance is not ported -- a script
/// sets the read-write `time` itself, the reference's auto-tick is the
/// per-frame delta the rest of the math surface leaves alone too.
mod v4animation {
    use super::*;

    /// The instance body beyond the `PyObject` header: the keyframe list and
    /// the two floats of the reference's `Vector4Animation`.
    #[repr(C)]
    struct AnimationBody {
        header: capi::PyObject,
        keyframes: Vec<(f32, *mut capi::PyObject)>,
        time: f32,
        duration: f32,
    }

    const INSTANCE_SIZE: usize =
        std::mem::size_of::<AnimationBody>() - std::mem::size_of::<capi::PyObject>();

    /// The one `Math.Vector4Animation` type object, by address.
    static TYPE_ADDR: AtomicUsize = AtomicUsize::new(0);

    fn type_addr() -> usize {
        TYPE_ADDR.load(Ordering::SeqCst)
    }

    /// The stored keyframe list of an instance.
    ///
    /// # Safety
    ///
    /// `self_` is a borrowed `Vector4Animation` instance, the type table's
    /// own handler receiver.
    unsafe fn keyframes_of<'a>(self_: *mut capi::PyObject) -> &'a Vec<(f32, *mut capi::PyObject)> {
        // SAFETY: the receiver is the animation body.
        unsafe { &*std::ptr::addr_of!((*self_.cast::<AnimationBody>()).keyframes) }
    }

    /// The output of a held provider, the reference's virtual call.
    fn held_output(provider: *mut capi::PyObject) -> [f32; 4] {
        v4provider::output_of(provider).unwrap_or([0.0; 4])
    }

    /// `Vector4Animation::output` (`script_math.cpp:3640-3671`).
    fn animation_output(object: *mut capi::PyObject) -> [f32; 4] {
        let keyframes = unsafe { keyframes_of(object) };
        if keyframes.is_empty() {
            return [0.0; 4];
        }
        // The reference walks the list once: `nit` is the first keyframe at
        // or after `time`, `oit` the one before it (`:3652-3657`). The
        // strict `<` leaves a keyframe at exactly `time` as the `nit` that
        // lerps to 1, and a time before the first one unadvanced.
        let time = unsafe { std::ptr::addr_of!((*object.cast::<AnimationBody>()).time).read() };
        let mut nit = 0;
        while nit < keyframes.len() && keyframes[nit].0 < time {
            nit += 1;
        }
        if nit == 0 {
            // `nit == oit`: before the first keyframe, its value.
            held_output(keyframes[0].1)
        } else if nit == keyframes.len() {
            // after the last keyframe, its value.
            held_output(keyframes[nit - 1].1)
        } else {
            // between `oit` and `nit`: the reference reads `oit` first,
            // then `nit`, and lerps with `XPVec4Lerp`.
            let oit = nit - 1;
            let ov = held_output(keyframes[oit].1);
            let nv = held_output(keyframes[nit].1);
            let t = (time - keyframes[oit].0) / (keyframes[nit].0 - keyframes[oit].0);
            let mut val = [0.0f32; 4];
            for index in 0..4 {
                val[index] = ov[index] + (nv[index] - ov[index]) * t;
            }
            val
        }
    }

    /// `PY_RW_ATTRIBUTE_DECLARE( time_, time )` read: `Script::getData(float)`.
    unsafe extern "C" fn time_get(
        self_: *mut capi::PyObject,
        _closure: *mut std::os::raw::c_void,
    ) -> *mut capi::PyObject {
        // SAFETY: the receiver is the animation body.
        let time = unsafe { std::ptr::addr_of!((*self_.cast::<AnimationBody>()).time).read() };
        unsafe { capi::PyFloat_FromDouble(time as f64) }
    }

    /// `time` write, the reference's own `pySet_time`: `Script::setData` for a
    /// float.
    unsafe extern "C" fn time_set(
        self_: *mut capi::PyObject,
        value: *mut capi::PyObject,
        _closure: *mut std::os::raw::c_void,
    ) -> c_int {
        let Some(v) = (unsafe { as_script_float(value) }) else {
            type_error("time must be set to a float");
            return -1;
        };
        // SAFETY: the receiver is the animation body.
        unsafe { std::ptr::addr_of_mut!((*self_.cast::<AnimationBody>()).time).write(v) };
        0
    }

    /// `duration` read, `PY_RW_ATTRIBUTE_DECLARE( duration_, duration )`.
    unsafe extern "C" fn duration_get(
        self_: *mut capi::PyObject,
        _closure: *mut std::os::raw::c_void,
    ) -> *mut capi::PyObject {
        // SAFETY: the receiver is the animation body.
        let duration =
            unsafe { std::ptr::addr_of!((*self_.cast::<AnimationBody>()).duration).read() };
        unsafe { capi::PyFloat_FromDouble(duration as f64) }
    }

    /// `duration` write, the reference's own `pySet_duration`.
    unsafe extern "C" fn duration_set(
        self_: *mut capi::PyObject,
        value: *mut capi::PyObject,
        _closure: *mut std::os::raw::c_void,
    ) -> c_int {
        let Some(v) = (unsafe { as_script_float(value) }) else {
            type_error("duration must be set to a float");
            return -1;
        };
        // SAFETY: the receiver is the animation body.
        unsafe { std::ptr::addr_of_mut!((*self_.cast::<AnimationBody>()).duration).write(v) };
        0
    }

    /// The reference passes `s_varNameValue_` ("An item of a PySTLSequence
    /// of <the pair's mangled type name>", `stl_to_py.hpp:316-335`) to the
    /// keyframe converter's `%s`; the port spells the list the mangled name
    /// stood for.
    const KEYFRAME_ERROR: &str = "An item of a PySTLSequence of keyframes must be set to a tuple \
                                  of a float and a Vector4Provider";

    /// The keyframe converter (`script_math.cpp:3433-3454`): a 2-tuple of a
    /// float and a `Vector4Provider`. The provider is `coerce`d, so a
    /// `Vector4` or a tuple of four floats becomes a `Vector4Basic`; `None`
    /// is refused too, the reference's `good = (ret == 0) && t.second`. Any
    /// failure is the one `TypeError`, the reference overwriting the
    /// sub-converter's own error. The caller owns the returned provider
    /// reference.
    ///
    /// # Safety
    ///
    /// `item` is a borrowed object the interpreter handed out, or null.
    fn as_keyframe(item: *mut capi::PyObject) -> Option<(f32, *mut capi::PyObject)> {
        // SAFETY: the checks read the borrowed item.
        if !unsafe { capi::py_tuple_check(item) } || unsafe { capi::PyTuple_Size(item) } != 2 {
            return None;
        }
        let time = unsafe { capi::PyTuple_GetItem(item, 0) };
        let provider = unsafe { capi::PyTuple_GetItem(item, 1) };
        let time = (unsafe { as_script_float(time) })?;
        let coerced = v4provider::coerce(provider);
        if !v4provider::provider_check(coerced) {
            if !coerced.is_null() {
                unsafe { capi::dec_ref(coerced) };
            }
            return None;
        }
        Some((time, coerced))
    }

    /// `Script::getData(const PySTLSequenceHolderBase&)` (`stl_to_py.cpp:481`)
    /// on the keyframes: the list of `(float, Vector4Provider)` tuples. The
    /// reference returns a live `PySTLSequence` view over the stored vector;
    /// the port returns a fresh list, which behaves the same for every read
    /// that does not mutate the returned object in place.
    unsafe extern "C" fn keyframes_get(
        self_: *mut capi::PyObject,
        _closure: *mut std::os::raw::c_void,
    ) -> *mut capi::PyObject {
        let keyframes = unsafe { keyframes_of(self_) };
        // SAFETY: the list and its tuple items are fresh references the
        // setters steal one by one.
        unsafe {
            let list = capi::PyList_New(keyframes.len() as capi::PySsize);
            if list.is_null() {
                return std::ptr::null_mut();
            }
            for (index, (time, provider)) in keyframes.iter().enumerate() {
                let value = capi::PyFloat_FromDouble(*time as f64);
                if value.is_null() {
                    capi::dec_ref(list);
                    return std::ptr::null_mut();
                }
                let tuple = capi::PyTuple_New(2);
                if tuple.is_null() {
                    capi::dec_ref(value);
                    capi::dec_ref(list);
                    return std::ptr::null_mut();
                }
                capi::PyTuple_SetItem(tuple, 0, value);
                if !provider.is_null() {
                    capi::inc_ref(*provider);
                }
                capi::PyTuple_SetItem(tuple, 1, *provider);
                capi::PyList_SetItem(list, index as capi::PySsize, tuple);
            }
            list
        }
    }

    /// `Script::setData(value, PySTLSequenceHolderBase&, "keyframes")`
    /// (`stl_to_py.cpp:422-478`): a sequence whose items are the keyframe
    /// converter's `(float, Vector4Provider)` tuples. The whole list is
    /// converted before it replaces the stored one, so a failed assignment
    /// leaves the original keyframes intact -- the reference's `cancel`
    /// restores them the same way, without the reference's dangling re-hold.
    unsafe extern "C" fn keyframes_set(
        self_: *mut capi::PyObject,
        value: *mut capi::PyObject,
        _closure: *mut std::os::raw::c_void,
    ) -> c_int {
        // `PySequence_Check` first, the reference's `%s must be set to a
        // sequence` arm (`stl_to_py.cpp:437-442`).
        if unsafe { capi::PySequence_Check(value) } == 0 {
            type_error("keyframes must be set to a sequence");
            return -1;
        }
        let size = unsafe { capi::PySequence_Size(value) };
        if size < 0 {
            // `PySequence_Size` left the error.
            return -1;
        }
        let mut staged: Vec<(f32, *mut capi::PyObject)> = Vec::new();
        for index in 0..size {
            let item = unsafe { capi::PySequence_GetItem(value, index) };
            let ok = if item.is_null() {
                false
            } else {
                let keyframe = as_keyframe(item);
                unsafe { capi::dec_ref(item) };
                if let Some(keyframe) = keyframe {
                    staged.push(keyframe);
                    true
                } else {
                    false
                }
            };
            if !ok {
                for (_, provider) in staged {
                    if !provider.is_null() {
                        unsafe { capi::dec_ref(provider) };
                    }
                }
                type_error(KEYFRAME_ERROR);
                return -1;
            }
        }
        // Commit: the staged list takes over, releasing the old occupants.
        let keyframes =
            unsafe { &mut *std::ptr::addr_of_mut!((*self_.cast::<AnimationBody>()).keyframes) };
        let old = std::mem::replace(keyframes, staged);
        for (_, provider) in old {
            if !provider.is_null() {
                unsafe { capi::dec_ref(provider) };
            }
        }
        0
    }

    /// The reference's `Vector4Animation` has no `tp_dealloc` of its own;
    /// the keyframe providers are released as the C++ member dies, then the
    /// instance goes back through the type's free function.
    unsafe extern "C" fn dealloc(self_: *mut capi::PyObject) {
        // SAFETY: only `Vector4Animation` instances reach its dealloc; the
        // vector is moved out so its buffer is freed by Rust's allocator as
        // it is consumed.
        let keyframes =
            unsafe { std::ptr::addr_of!((*self_.cast::<AnimationBody>()).keyframes).read() };
        for (_, provider) in keyframes {
            if !provider.is_null() {
                unsafe { capi::dec_ref(provider) };
            }
        }
        // SAFETY: as `provider_dealloc!`.
        unsafe { capi::PyObject_Free(self_.cast()) };
    }

    /// A fresh `Math.Vector4Animation`; the caller owns the reference.
    fn make() -> *mut capi::PyObject {
        let Ok(type_object) = binding::create_type(&spec()) else {
            return std::ptr::null_mut();
        };
        let Some(instance) = (unsafe { binding::instantiate(&type_object) }) else {
            return std::ptr::null_mut();
        };
        let pointer = instance.as_ptr();
        // SAFETY: the allocated body is exactly an `AnimationBody`; the
        // fields start at the reference's constructor values
        // (`script_math.cpp:3666-3671`): an empty keyframe list, zero time
        // and a one-second duration.
        unsafe {
            std::ptr::addr_of_mut!((*pointer.cast::<AnimationBody>()).keyframes).write(Vec::new());
            std::ptr::addr_of_mut!((*pointer.cast::<AnimationBody>()).time).write(0.0);
            std::ptr::addr_of_mut!((*pointer.cast::<AnimationBody>()).duration).write(1.0);
        }
        std::mem::forget(instance);
        pointer
    }

    /// The `_pyNew`-shape factory of `PY_FACTORY( Vector4Animation, Math )`:
    /// a fresh animation, the reference's `END` argument list meaning no
    /// arguments.
    unsafe extern "C" fn factory(
        _module: *mut capi::PyObject,
        args: *mut capi::PyObject,
    ) -> *mut capi::PyObject {
        if !want_args(args, 0) {
            return type_error("Math.Vector4Animation() takes no arguments");
        }
        make()
    }

    /// The factory descriptor for `PyCFunction_New`; leaked for the type's
    /// lifetime, the reference's static method table.
    fn factory_def() -> *mut capi::PyMethodDef {
        Box::leak(Box::new(capi::PyMethodDef {
            ml_name: c"Vector4Animation".as_ptr() as *const c_char,
            ml_meth: Some(factory),
            ml_flags: capi::METH_VARARGS,
            ml_doc: std::ptr::null(),
        }))
    }

    /// The `PY_BEGIN_ATTRIBUTES` table of `Vector4Animation` and its
    /// destruction entry: the keyframe sequence and the two floats, the
    /// reference's `PY_RW_ATTRIBUTE_DECLARE` pair.
    fn spec() -> binding::TypeSpec {
        binding::TypeSpec::new("Math.Vector4Animation")
            .base(v4provider::base_addr())
            .attribute("keyframes", keyframes_get, Some(keyframes_set))
            .attribute("time", time_get, Some(time_set))
            .attribute("duration", duration_get, Some(duration_set))
            .dealloc(dealloc)
            .instance_size(INSTANCE_SIZE)
    }

    /// `TempPyVectorInit` for the animation: the type object, its dispatch
    /// entry, and the factory into the `Math` module dictionary.
    pub(super) fn register() -> Result<PyRef, binding::BindError> {
        let type_object = binding::create_type(&spec())?;
        TYPE_ADDR.store(type_object.as_ptr() as usize, Ordering::SeqCst);
        v4provider::register_output(type_addr(), animation_output);

        // SAFETY: `register` runs from interpreter init; the interpreter is up.
        unsafe {
            let module = capi::PyImport_AddModule(c"Math".as_ptr());
            if !module.is_null() {
                let dict = capi::PyModule_GetDict(module);
                let factory = capi::PyCFunction_New(factory_def(), std::ptr::null_mut());
                if !factory.is_null() {
                    capi::PyDict_SetItemString(dict, c"Vector4Animation".as_ptr(), factory);
                    capi::dec_ref(factory);
                }
            }
        }
        Ok(type_object)
    }
}

/// The `Math.Vector4Morph` provider (`script_math.cpp:3696-3868`): the value
/// that morphs from where it was to the target over the duration.
///
/// The reference's `output` is `(t * target) + ((1-t) * old)` at
/// `t = time / duration` (`:3802-3809`); setting a new target snapshots the
/// current output into the morph source and restarts the timer
/// (`:3830-3838`), and the duration setter keeps the fraction through the
/// cycle (`:3845-3857`). The clamps follow `Math::clamp`
/// (`math/mathdef.hpp:223-227`) and `std::max( 0.0001f, d )` exactly, a
/// `NaN` passing through the clamp and pinning the duration at its minimum.
/// The `tick` advance is not ported, the per-frame delta the rest of the
/// math surface leaves alone too.
mod v4morph {
    use super::*;

    /// The duration floor of `Vector4Morph::duration` and `time`.
    const MIN_DURATION: f32 = 0.0001;

    /// The instance body beyond the `PyObject` header: the target, the two
    /// floats and the morph source, the layout of the reference's
    /// `Vector4Morph`.
    #[repr(C)]
    struct MorphBody {
        header: capi::PyObject,
        target: [f32; 4],
        duration: f32,
        time: f32,
        old: [f32; 4],
    }

    const INSTANCE_SIZE: usize =
        std::mem::size_of::<MorphBody>() - std::mem::size_of::<capi::PyObject>();

    /// The one `Math.Vector4Morph` type object, by address.
    static TYPE_ADDR: AtomicUsize = AtomicUsize::new(0);

    fn type_addr() -> usize {
        TYPE_ADDR.load(Ordering::SeqCst)
    }

    /// `Vector4Morph::output` (`script_math.cpp:3802-3809`).
    fn morph_output(object: *mut capi::PyObject) -> [f32; 4] {
        let target = unsafe { std::ptr::addr_of!((*object.cast::<MorphBody>()).target).read() };
        let duration = unsafe { std::ptr::addr_of!((*object.cast::<MorphBody>()).duration).read() };
        let time = unsafe { std::ptr::addr_of!((*object.cast::<MorphBody>()).time).read() };
        let old = unsafe { std::ptr::addr_of!((*object.cast::<MorphBody>()).old).read() };
        let t = time / duration;
        let mut val = [0.0f32; 4];
        for index in 0..4 {
            val[index] = t * target[index] + (1.0 - t) * old[index];
        }
        val
    }

    /// `Vector4Morph::pyGet_target` (`:3759`).
    unsafe extern "C" fn get_target(
        self_: *mut capi::PyObject,
        _closure: *mut std::os::raw::c_void,
    ) -> *mut capi::PyObject {
        let target = unsafe { std::ptr::addr_of!((*self_.cast::<MorphBody>()).target).read() };
        vector4::make(target)
    }

    /// `Vector4Morph::pySet_target` and `target()` (`:3830-3838`): the
    /// current output becomes the morph source, the target moves and the
    /// timer restarts.
    unsafe extern "C" fn set_target(
        self_: *mut capi::PyObject,
        value: *mut capi::PyObject,
        _closure: *mut std::os::raw::c_void,
    ) -> c_int {
        let Some(target) = (unsafe { as_vector::<4>(value, vector4::type_addr()) }) else {
            type_error("target must be set to a Vector4 or tuple of 4 floats");
            return -1;
        };
        let current = morph_output(self_);
        // SAFETY: the receiver is the morph body.
        unsafe {
            std::ptr::addr_of_mut!((*self_.cast::<MorphBody>()).old).write(current);
            std::ptr::addr_of_mut!((*self_.cast::<MorphBody>()).target).write(target);
            std::ptr::addr_of_mut!((*self_.cast::<MorphBody>()).time).write(0.0);
        }
        0
    }

    /// `Vector4Morph::duration` (`:3845-3857`): the new duration has a
    /// minimum, and the fraction through the old cycle is kept.
    unsafe extern "C" fn set_duration(
        self_: *mut capi::PyObject,
        value: *mut capi::PyObject,
        _closure: *mut std::os::raw::c_void,
    ) -> c_int {
        let Some(d) = (unsafe { as_script_float(value) }) else {
            type_error("duration must be set to a float");
            return -1;
        };
        let old_duration =
            unsafe { std::ptr::addr_of!((*self_.cast::<MorphBody>()).duration).read() };
        let time = unsafe { std::ptr::addr_of!((*self_.cast::<MorphBody>()).time).read() };
        // `std::max( 0.0001f, d )`: a `NaN` fails the comparison and pins
        // the duration at the minimum.
        let new_duration = if MIN_DURATION < d { d } else { MIN_DURATION };
        let fraction = time / old_duration;
        // The reference goes through `time()`, whose clamp is
        // `Math::clamp( 0.0001f, t, duration_ )`.
        let new_time = if fraction * new_duration < MIN_DURATION {
            MIN_DURATION
        } else if fraction * new_duration > new_duration {
            new_duration
        } else {
            fraction * new_duration
        };
        // SAFETY: the receiver is the morph body.
        unsafe {
            std::ptr::addr_of_mut!((*self_.cast::<MorphBody>()).duration).write(new_duration);
            std::ptr::addr_of_mut!((*self_.cast::<MorphBody>()).time).write(new_time);
        }
        0
    }

    /// `Vector4Morph::time` (`:3862-3867`): the timer, clamped into the
    /// cycle.
    unsafe extern "C" fn set_time(
        self_: *mut capi::PyObject,
        value: *mut capi::PyObject,
        _closure: *mut std::os::raw::c_void,
    ) -> c_int {
        let Some(t) = (unsafe { as_script_float(value) }) else {
            type_error("time must be set to a float");
            return -1;
        };
        let duration = unsafe { std::ptr::addr_of!((*self_.cast::<MorphBody>()).duration).read() };
        let clamped = if t < MIN_DURATION {
            MIN_DURATION
        } else if t > duration {
            duration
        } else {
            t
        };
        // SAFETY: the receiver is the morph body.
        unsafe { std::ptr::addr_of_mut!((*self_.cast::<MorphBody>()).time).write(clamped) };
        0
    }

    /// `duration` read, `PY_RW_ACCESSOR_ATTRIBUTE_DECLARE( float, duration,
    /// duration )`.
    unsafe extern "C" fn get_duration(
        self_: *mut capi::PyObject,
        _closure: *mut std::os::raw::c_void,
    ) -> *mut capi::PyObject {
        // SAFETY: the receiver is the morph body.
        let duration = unsafe { std::ptr::addr_of!((*self_.cast::<MorphBody>()).duration).read() };
        unsafe { capi::PyFloat_FromDouble(duration as f64) }
    }

    /// `time` read, `PY_RW_ACCESSOR_ATTRIBUTE_DECLARE( float, time, time )`.
    unsafe extern "C" fn get_time(
        self_: *mut capi::PyObject,
        _closure: *mut std::os::raw::c_void,
    ) -> *mut capi::PyObject {
        // SAFETY: the receiver is the morph body.
        let time = unsafe { std::ptr::addr_of!((*self_.cast::<MorphBody>()).time).read() };
        unsafe { capi::PyFloat_FromDouble(time as f64) }
    }

    /// A fresh `Math.Vector4Morph` morphing from `orig`; the caller owns the
    /// reference. The constructor's field values are the reference's
    /// (`:3709-3713`): the source starts at the given value with a
    /// one-second cycle and a zero timer.
    fn make(orig: [f32; 4]) -> *mut capi::PyObject {
        let Ok(type_object) = binding::create_type(&spec()) else {
            return std::ptr::null_mut();
        };
        let Some(instance) = (unsafe { binding::instantiate(&type_object) }) else {
            return std::ptr::null_mut();
        };
        let pointer = instance.as_ptr();
        // SAFETY: the allocated body is exactly a `MorphBody`.
        unsafe {
            std::ptr::addr_of_mut!((*pointer.cast::<MorphBody>()).target).write(orig);
            std::ptr::addr_of_mut!((*pointer.cast::<MorphBody>()).duration).write(1.0);
            std::ptr::addr_of_mut!((*pointer.cast::<MorphBody>()).time).write(0.0);
            std::ptr::addr_of_mut!((*pointer.cast::<MorphBody>()).old).write(orig);
        }
        std::mem::forget(instance);
        pointer
    }

    /// The `PY_AUTO_FACTORY_DECLARE( Vector4Morph, OPTARG( Vector4, END ) )`
    /// factory: a fresh morph from the optional `Vector4`.
    unsafe extern "C" fn factory(
        _module: *mut capi::PyObject,
        args: *mut capi::PyObject,
    ) -> *mut capi::PyObject {
        let count = unsafe { capi::PyTuple_Size(args) };
        let orig = match count {
            0 => [0.0; 4],
            1 => match unsafe { as_vector::<4>(arg(args, 0), vector4::type_addr()) } {
                Some(orig) => orig,
                None => return type_error("Math.Vector4Morph(): Expected an optional Vector4"),
            },
            _ => return type_error("Math.Vector4Morph(): Expected an optional Vector4"),
        };
        make(orig)
    }

    /// The factory descriptor for `PyCFunction_New`; leaked for the type's
    /// lifetime, the reference's static method table.
    fn factory_def() -> *mut capi::PyMethodDef {
        Box::leak(Box::new(capi::PyMethodDef {
            ml_name: c"Vector4Morph".as_ptr() as *const c_char,
            ml_meth: Some(factory),
            ml_flags: capi::METH_VARARGS,
            ml_doc: std::ptr::null(),
        }))
    }

    /// The `PY_BEGIN_ATTRIBUTES` table of `Vector4Morph`: the target, the
    /// duration and the timer, the reference's accessor attribute pair.
    fn spec() -> binding::TypeSpec {
        binding::TypeSpec::new("Math.Vector4Morph")
            .base(v4provider::base_addr())
            .attribute("target", get_target, Some(set_target))
            .attribute("duration", get_duration, Some(set_duration))
            .attribute("time", get_time, Some(set_time))
            .instance_size(INSTANCE_SIZE)
    }

    /// `TempPyVectorInit` for the morph: the type object, its dispatch
    /// entry, and the factory into the `Math` module dictionary.
    pub(super) fn register() -> Result<PyRef, binding::BindError> {
        let type_object = binding::create_type(&spec())?;
        TYPE_ADDR.store(type_object.as_ptr() as usize, Ordering::SeqCst);
        v4provider::register_output(type_addr(), morph_output);

        // SAFETY: `register` runs from interpreter init; the interpreter is up.
        unsafe {
            let module = capi::PyImport_AddModule(c"Math".as_ptr());
            if !module.is_null() {
                let dict = capi::PyModule_GetDict(module);
                let factory = capi::PyCFunction_New(factory_def(), std::ptr::null_mut());
                if !factory.is_null() {
                    capi::PyDict_SetItemString(dict, c"Vector4Morph".as_ptr(), factory);
                    capi::dec_ref(factory);
                }
            }
        }
        Ok(type_object)
    }
}

/// The `Math.Vector4Translation` provider
/// (`script_math.cpp:3889-3966`): the translation part of a
/// `MatrixProvider`, a subclass of `Vector4Provider`.
///
/// The reference's `output` dereferences its `source_` unconditionally
/// (`:3962`), so a translation with an empty slot would trap; the port yields
/// the zero translation the reference's `v.set( tr.x, tr.y, tr.z, 1.f )`
/// writes for a zero origin, a defined value for the reference's undefined
/// case. The factory's converter likewise refuses a `None` slot the
/// reference would take and then trap on.
mod v4translation {
    use super::*;

    /// The instance body beyond the `PyObject` header: the one held
    /// `MatrixProvider`, the layout of the reference's `Vector4Translation`.
    #[repr(C)]
    struct TranslationBody {
        header: capi::PyObject,
        source: *mut capi::PyObject,
    }

    const SOURCE: usize = std::mem::offset_of!(TranslationBody, source);

    const INSTANCE_SIZE: usize =
        std::mem::size_of::<TranslationBody>() - std::mem::size_of::<capi::PyObject>();

    /// The one `Math.Vector4Translation` type object, by address.
    static TYPE_ADDR: AtomicUsize = AtomicUsize::new(0);

    fn type_addr() -> usize {
        TYPE_ADDR.load(Ordering::SeqCst)
    }

    /// `Vector4Translation::output` (`script_math.cpp:3960-3966`).
    fn translation_output(object: *mut capi::PyObject) -> [f32; 4] {
        let Some(matrix) = matrix::provider_matrix(unsafe { matrix::slot_read::<SOURCE>(object) })
        else {
            return [0.0, 0.0, 0.0, 1.0];
        };
        let origin = matrix.apply_to_origin();
        [origin.x, origin.y, origin.z, 1.0]
    }

    /// A fresh `Math.Vector4Translation` over `source`; the caller owns the
    /// returned reference, and the field takes over `source`, the reference's
    /// `source_( s )` member initialiser.
    fn make(source: *mut capi::PyObject) -> *mut capi::PyObject {
        let Ok(type_object) = binding::create_type(&spec()) else {
            return std::ptr::null_mut();
        };
        let Some(instance) = (unsafe { binding::instantiate(&type_object) }) else {
            return std::ptr::null_mut();
        };
        let pointer = instance.as_ptr();
        // SAFETY: the allocated body is exactly a `TranslationBody`.
        unsafe {
            std::ptr::addr_of_mut!((*pointer.cast::<TranslationBody>()).source).write(source);
        }
        std::mem::forget(instance);
        pointer
    }

    /// The `PY_AUTO_FACTORY_DECLARE( Vector4Translation,
    /// ARG(MatrixProviderPtr, END) )` factory: a fresh translation over the
    /// one `MatrixProvider`.
    unsafe extern "C" fn factory(
        _module: *mut capi::PyObject,
        args: *mut capi::PyObject,
    ) -> *mut capi::PyObject {
        if !want_args(args, 1) {
            return type_error(
                "Math.Vector4Translation() expects 1 argument of type MatrixProvider",
            );
        }
        let source = arg(args, 0);
        if !matrix::provider_check(source) {
            // `Script::setData( pObject, MatrixProviderPtr&, "() argument
            // 1" )` (`script.hpp:610-631`): the reference takes a `None` here
            // and traps on the first `output`, so the port refuses it with
            // the converter's own error.
            return type_error("() argument 1 must be set to a MatrixProvider or None");
        }
        // The field owns the reference it is given.
        unsafe { capi::inc_ref(source) };
        make(source)
    }

    /// The factory descriptor for `PyCFunction_New`; leaked for the type's
    /// lifetime, the reference's static method table.
    fn factory_def() -> *mut capi::PyMethodDef {
        Box::leak(Box::new(capi::PyMethodDef {
            ml_name: c"Vector4Translation".as_ptr() as *const c_char,
            ml_meth: Some(factory),
            ml_flags: capi::METH_VARARGS,
            ml_doc: std::ptr::null(),
        }))
    }

    /// The `PY_BEGIN_ATTRIBUTES` table of `Vector4Translation` and its
    /// destruction entry: the one held `MatrixProvider`, the reference's
    /// `PY_RW_ATTRIBUTE_DECLARE( source_, source )` converter pair.
    fn spec() -> binding::TypeSpec {
        binding::TypeSpec::new("Math.Vector4Translation")
            .base(v4provider::base_addr())
            .attribute_with(
                "source",
                matrix::slot_get::<SOURCE>,
                Some(matrix::slot_set::<SOURCE>),
                c"source".as_ptr() as *mut std::os::raw::c_void,
            )
            .dealloc(dealloc)
            .instance_size(INSTANCE_SIZE)
    }

    provider_dealloc!(dealloc, TranslationBody, source);

    /// `TempPyVectorInit` for the translation: the type object, its dispatch
    /// entry, and the factory into the `Math` module dictionary.
    pub(super) fn register() -> Result<PyRef, binding::BindError> {
        let type_object = binding::create_type(&spec())?;
        TYPE_ADDR.store(type_object.as_ptr() as usize, Ordering::SeqCst);
        v4provider::register_output(type_addr(), translation_output);

        // SAFETY: `register` runs from interpreter init; the interpreter is up.
        unsafe {
            let module = capi::PyImport_AddModule(c"Math".as_ptr());
            if !module.is_null() {
                let dict = capi::PyModule_GetDict(module);
                let factory = capi::PyCFunction_New(factory_def(), std::ptr::null_mut());
                if !factory.is_null() {
                    capi::PyDict_SetItemString(dict, c"Vector4Translation".as_ptr(), factory);
                    capi::dec_ref(factory);
                }
            }
        }
        Ok(type_object)
    }
}

/// The `Math.Vector4Distance` provider
/// (`script_math.cpp:3972-4062`): the distance between the two
/// `MatrixProvider` origins, on every component.
///
/// As with the translation, the reference's `output` dereferences both
/// origins unconditionally (`:4058-4059`), so an empty slot would trap; the
/// port treats an empty or non-provider slot as the zero origin, the origin
/// of the reference's `Matrix ma;` default. The two slots are read in the
/// reference's order, `a` then `b`, before the difference.
mod v4distance {
    use super::*;

    /// The instance body beyond the `PyObject` header: the two held
    /// `MatrixProvider`s, the layout of the reference's `Vector4Distance`.
    #[repr(C)]
    struct DistanceBody {
        header: capi::PyObject,
        a: *mut capi::PyObject,
        b: *mut capi::PyObject,
    }

    const A: usize = std::mem::offset_of!(DistanceBody, a);
    const B: usize = std::mem::offset_of!(DistanceBody, b);

    const INSTANCE_SIZE: usize =
        std::mem::size_of::<DistanceBody>() - std::mem::size_of::<capi::PyObject>();

    /// The one `Math.Vector4Distance` type object, by address.
    static TYPE_ADDR: AtomicUsize = AtomicUsize::new(0);

    fn type_addr() -> usize {
        TYPE_ADDR.load(Ordering::SeqCst)
    }

    /// `Vector4Distance::output` (`script_math.cpp:4055-4062`).
    fn distance_output(object: *mut capi::PyObject) -> [f32; 4] {
        let origin_a = matrix::provider_matrix(unsafe { matrix::slot_read::<A>(object) })
            .map(|m| m.apply_to_origin())
            .unwrap_or(foundation::math::Vector3::new(0.0, 0.0, 0.0));
        let origin_b = matrix::provider_matrix(unsafe { matrix::slot_read::<B>(object) })
            .map(|m| m.apply_to_origin())
            .unwrap_or(foundation::math::Vector3::new(0.0, 0.0, 0.0));
        let distance = (origin_a - origin_b).length();
        [distance, distance, distance, distance]
    }

    /// A fresh `Math.Vector4Distance` between the two providers; the caller
    /// owns the returned reference, and the fields take over `a` and `b`,
    /// the reference's constructor member initialisers.
    fn make(a: *mut capi::PyObject, b: *mut capi::PyObject) -> *mut capi::PyObject {
        let Ok(type_object) = binding::create_type(&spec()) else {
            return std::ptr::null_mut();
        };
        let Some(instance) = (unsafe { binding::instantiate(&type_object) }) else {
            return std::ptr::null_mut();
        };
        let pointer = instance.as_ptr();
        // SAFETY: the allocated body is exactly a `DistanceBody`.
        unsafe {
            std::ptr::addr_of_mut!((*pointer.cast::<DistanceBody>()).a).write(a);
            std::ptr::addr_of_mut!((*pointer.cast::<DistanceBody>()).b).write(b);
        }
        std::mem::forget(instance);
        pointer
    }

    /// The `PY_AUTO_FACTORY_DECLARE( Vector4Distance,
    /// ARG(MatrixProviderPtr, ARG(MatrixProviderPtr, END) ) )` factory: a
    /// fresh distance between the two `MatrixProvider`s, arg 1 converted
    /// before arg 2 as the reference's `PYAUTO_WRITE` order goes.
    unsafe extern "C" fn factory(
        _module: *mut capi::PyObject,
        args: *mut capi::PyObject,
    ) -> *mut capi::PyObject {
        if !want_args(args, 2) {
            return type_error("Math.Vector4Distance() expects 2 arguments of type MatrixProvider");
        }
        let a = arg(args, 0);
        if !matrix::provider_check(a) {
            // The reference takes a `None` here and traps on the first
            // `output`, so the port refuses it with the converter's own error.
            return type_error("() argument 1 must be set to a MatrixProvider or None");
        }
        let b = arg(args, 1);
        if !matrix::provider_check(b) {
            return type_error("() argument 2 must be set to a MatrixProvider or None");
        }
        // The fields own the references they are given.
        unsafe {
            capi::inc_ref(a);
            capi::inc_ref(b);
        }
        make(a, b)
    }

    /// The factory descriptor for `PyCFunction_New`; leaked for the type's
    /// lifetime, the reference's static method table.
    fn factory_def() -> *mut capi::PyMethodDef {
        Box::leak(Box::new(capi::PyMethodDef {
            ml_name: c"Vector4Distance".as_ptr() as *const c_char,
            ml_meth: Some(factory),
            ml_flags: capi::METH_VARARGS,
            ml_doc: std::ptr::null(),
        }))
    }

    /// The `PY_BEGIN_ATTRIBUTES` table of `Vector4Distance` and its
    /// destruction entry: the two held `MatrixProvider`s, the reference's
    /// `PY_RW_ATTRIBUTE_DECLARE` converter pairs.
    fn spec() -> binding::TypeSpec {
        binding::TypeSpec::new("Math.Vector4Distance")
            .base(v4provider::base_addr())
            .attribute_with(
                "a",
                matrix::slot_get::<A>,
                Some(matrix::slot_set::<A>),
                c"a".as_ptr() as *mut std::os::raw::c_void,
            )
            .attribute_with(
                "b",
                matrix::slot_get::<B>,
                Some(matrix::slot_set::<B>),
                c"b".as_ptr() as *mut std::os::raw::c_void,
            )
            .dealloc(dealloc)
            .instance_size(INSTANCE_SIZE)
    }

    provider_dealloc!(dealloc, DistanceBody, a, b);

    /// `TempPyVectorInit` for the distance: the type object, its dispatch
    /// entry, and the factory into the `Math` module dictionary.
    pub(super) fn register() -> Result<PyRef, binding::BindError> {
        let type_object = binding::create_type(&spec())?;
        TYPE_ADDR.store(type_object.as_ptr() as usize, Ordering::SeqCst);
        v4provider::register_output(type_addr(), distance_output);

        // SAFETY: `register` runs from interpreter init; the interpreter is up.
        unsafe {
            let module = capi::PyImport_AddModule(c"Math".as_ptr());
            if !module.is_null() {
                let dict = capi::PyModule_GetDict(module);
                let factory = capi::PyCFunction_New(factory_def(), std::ptr::null_mut());
                if !factory.is_null() {
                    capi::PyDict_SetItemString(dict, c"Vector4Distance".as_ptr(), factory);
                    capi::dec_ref(factory);
                }
            }
        }
        Ok(type_object)
    }
}

/// The `Math.Vector4LFO` provider (`script_math.cpp:4066-4304`): the Low
/// Frequency Oscillator, one wave value on every component.
///
/// The four waveforms are the reference's `Waveform` enum, a string
/// converter (`script.hpp:830-886`): the getter returns the name, the setter
/// takes it, a value that is not a string is a `TypeError` while a string
/// that names no wave is a `ValueError`, both with the reference's
/// "must be set to one of" message. The `output` keeps the reference's
/// side effect: the square, triangle and sawtooth waves fold `time` into
/// the current cycle. The reference's `tick` advance -- and the square
/// wave's `parity_` it drives -- is not ported; a script sets the
/// read-write `time` itself, the lane's standing delta.
mod v4lfo {
    use super::*;

    /// The `Waveform` names in enum order (`script_math.cpp:4077-4090`):
    /// the map's index is the enum value, `WAVE_SQUARE` first.
    const WAVEFORMS: [&str; 4] = ["SQUARE", "TRIANGLE", "SINE", "SAWTOOTH"];

    /// `almostZero`'s default epsilon (`stdmf.hpp:616-619`).
    const ALMOST_ZERO: f32 = 0.0004;

    /// The instance body beyond the `PyObject` header: the wave, its four
    /// floats, the layout of the reference's `Vector4LFO` minus the
    /// unported `parity_` counter.
    #[repr(C)]
    struct LfoBody {
        header: capi::PyObject,
        waveform: u8,
        period: f32,
        amplitude: f32,
        phase: f32,
        time: f32,
    }

    const INSTANCE_SIZE: usize =
        std::mem::size_of::<LfoBody>() - std::mem::size_of::<capi::PyObject>();

    /// The one `Math.Vector4LFO` type object, by address.
    static TYPE_ADDR: AtomicUsize = AtomicUsize::new(0);

    fn type_addr() -> usize {
        TYPE_ADDR.load(Ordering::SeqCst)
    }

    /// `Vector4LFO::output` (`script_math.cpp:4240-4304`). Only registered
    /// for this type, so the receiver is an `LfoBody`.
    fn lfo_output(object: *mut capi::PyObject) -> [f32; 4] {
        // SAFETY: the receiver is an `LfoBody`.
        let (waveform, period, amplitude, phase, time) = unsafe {
            let body = &*object.cast::<LfoBody>();
            (
                body.waveform,
                body.period,
                body.amplitude,
                body.phase,
                body.time,
            )
        };
        // `almostZero( period_ )`: the wave is flat at a zero period, and
        // the `time` fold is skipped -- its value is left alone.
        let x = if -ALMOST_ZERO < period && period < ALMOST_ZERO {
            0.0
        } else {
            match waveform {
                // WAVE_SQUARE
                0 => {
                    let folded = fold_time(object, time, period);
                    let t = folded + phase;
                    // `period_ > 0` reads the half-cycle, else `parity_%2` --
                    // advanced only by the unported `tick`, so always zero.
                    let positive = period > 0.0 && t - period / 2.0 > 0.0;
                    if positive { amplitude } else { 0.0 }
                }
                // WAVE_TRIANGLE
                1 => {
                    let folded = fold_time(object, time, period);
                    let t = folded + phase;
                    let f = 1.0 / period;
                    let positive = t - period / 2.0 > 0.0;
                    let mut x = (t * f) % 1.0;
                    if !positive {
                        x = 1.0 - x;
                    }
                    x * amplitude
                }
                // WAVE_SINE
                2 => {
                    let t = (time + phase) % period;
                    let t = (t / period) * 2.0 * foundation::math::MATH_PI;
                    (t.sin() * 0.5 + 0.5) * amplitude
                }
                // WAVE_SAWTOOTH
                3 => {
                    let folded = fold_time(object, time, period);
                    let t = folded + phase;
                    let f = 1.0 / period;
                    ((t * f) % 1.0) * amplitude
                }
                // A wave value outside the enum leaves the reference's
                // switch with `x` still zero.
                _ => 0.0,
            }
        };
        [x, x, x, x]
    }

    /// The `time_ = fmodf( time_, period_ )` fold three of the four waves
    /// perform before they read the phase.
    fn fold_time(object: *mut capi::PyObject, time: f32, period: f32) -> f32 {
        let folded = time % period;
        // SAFETY: the receiver is an `LfoBody`.
        unsafe {
            std::ptr::addr_of_mut!((*object.cast::<LfoBody>()).time).write(folded);
        }
        folded
    }

    /// The float attribute pair the reference's `PY_RW_ATTRIBUTE_DECLARE`
    /// makes: `Script::getData(float)` back, `Script::setData(float)` in.
    macro_rules! lfo_float_attribute {
        ($get:ident, $set:ident, $field:ident, $name:literal) => {
            unsafe extern "C" fn $get(
                self_: *mut capi::PyObject,
                _closure: *mut std::os::raw::c_void,
            ) -> *mut capi::PyObject {
                // SAFETY: the receiver is an `LfoBody`.
                let value = unsafe { std::ptr::addr_of!((*self_.cast::<LfoBody>()).$field).read() };
                unsafe { capi::PyFloat_FromDouble(value as f64) }
            }

            unsafe extern "C" fn $set(
                self_: *mut capi::PyObject,
                value: *mut capi::PyObject,
                _closure: *mut std::os::raw::c_void,
            ) -> c_int {
                let Some(value) = (unsafe { as_script_float(value) }) else {
                    type_error(concat!($name, " must be set to a float"));
                    return -1;
                };
                // SAFETY: the receiver is an `LfoBody`.
                unsafe {
                    std::ptr::addr_of_mut!((*self_.cast::<LfoBody>()).$field).write(value);
                }
                0
            }
        };
    }

    lfo_float_attribute!(get_period, set_period, period, "period");
    lfo_float_attribute!(get_amplitude, set_amplitude, amplitude, "amplitude");
    lfo_float_attribute!(get_phase, set_phase, phase, "phase");
    lfo_float_attribute!(get_time, set_time, time, "time");

    /// `Vector4LFO::pyGet_waveform` (`:4130`, the enum `getData` at
    /// `script.hpp:872-886`): the wave's name.
    unsafe extern "C" fn get_waveform(
        self_: *mut capi::PyObject,
        _closure: *mut std::os::raw::c_void,
    ) -> *mut capi::PyObject {
        // SAFETY: the receiver is an `LfoBody`.
        let index = unsafe { std::ptr::addr_of!((*self_.cast::<LfoBody>()).waveform).read() };
        match WAVEFORMS.get(index as usize) {
            Some(name) => unsafe { capi::unicode_from_bytes(name.as_bytes()) },
            // The out-of-range arm the reference's converter leaves for a
            // corrupted enum value.
            None => capi::py_none(),
        }
    }

    /// `Vector4LFO::pySet_waveform` (the enum `setData` at
    /// `script.hpp:830-869`): one of the four names, `ValueError` for a
    /// string that names no wave, `TypeError` for anything else.
    unsafe extern "C" fn set_waveform(
        self_: *mut capi::PyObject,
        value: *mut capi::PyObject,
        _closure: *mut std::os::raw::c_void,
    ) -> c_int {
        let is_string = unsafe { capi::py_unicode_check(value) };
        if is_string {
            let name = unsafe { capi::to_string(value) };
            if let Some(index) = WAVEFORMS.iter().position(|wave| *wave == name) {
                // SAFETY: the receiver is an `LfoBody`.
                unsafe {
                    std::ptr::addr_of_mut!((*self_.cast::<LfoBody>()).waveform).write(index as u8);
                }
                return 0;
            }
        }
        // `PyErr_SetString( PyUnicode_Check(...) ? ValueError : TypeError )`.
        let message = "waveform must be set to one of SQUARE, TRIANGLE, SINE, or SAWTOOTH";
        if is_string {
            value_error(message);
        } else {
            type_error(message);
        }
        -1
    }

    /// A fresh `Math.Vector4LFO`; the caller owns the reference. The
    /// constructor's field values are the reference's (`:4092-4100`): the
    /// sine wave, a one-second period, unit amplitude and a zero phase and
    /// timer.
    fn make() -> *mut capi::PyObject {
        let Ok(type_object) = binding::create_type(&spec()) else {
            return std::ptr::null_mut();
        };
        let Some(instance) = (unsafe { binding::instantiate(&type_object) }) else {
            return std::ptr::null_mut();
        };
        let pointer = instance.as_ptr();
        // SAFETY: the allocated body is exactly an `LfoBody`.
        unsafe {
            std::ptr::addr_of_mut!((*pointer.cast::<LfoBody>()).waveform).write(2);
            std::ptr::addr_of_mut!((*pointer.cast::<LfoBody>()).period).write(1.0);
            std::ptr::addr_of_mut!((*pointer.cast::<LfoBody>()).amplitude).write(1.0);
            std::ptr::addr_of_mut!((*pointer.cast::<LfoBody>()).phase).write(0.0);
            std::ptr::addr_of_mut!((*pointer.cast::<LfoBody>()).time).write(0.0);
        }
        std::mem::forget(instance);
        pointer
    }

    /// The `PY_AUTO_FACTORY_DECLARE( Vector4LFO, END )` factory: a fresh
    /// oscillator, the reference's `END` argument list meaning no arguments.
    unsafe extern "C" fn factory(
        _module: *mut capi::PyObject,
        args: *mut capi::PyObject,
    ) -> *mut capi::PyObject {
        if !want_args(args, 0) {
            return type_error("Math.Vector4LFO() takes no arguments");
        }
        make()
    }

    /// The factory descriptor for `PyCFunction_New`; leaked for the type's
    /// lifetime, the reference's static method table.
    fn factory_def() -> *mut capi::PyMethodDef {
        Box::leak(Box::new(capi::PyMethodDef {
            ml_name: c"Vector4LFO".as_ptr() as *const c_char,
            ml_meth: Some(factory),
            ml_flags: capi::METH_VARARGS,
            ml_doc: std::ptr::null(),
        }))
    }

    /// The `PY_BEGIN_ATTRIBUTES` table of `Vector4LFO`: the four floats and
    /// the wave name, the reference's `PY_RW_ATTRIBUTE_DECLARE` pairs.
    fn spec() -> binding::TypeSpec {
        binding::TypeSpec::new("Math.Vector4LFO")
            .base(v4provider::base_addr())
            .attribute("period", get_period, Some(set_period))
            .attribute("amplitude", get_amplitude, Some(set_amplitude))
            .attribute("phase", get_phase, Some(set_phase))
            .attribute("time", get_time, Some(set_time))
            .attribute("waveform", get_waveform, Some(set_waveform))
            .instance_size(INSTANCE_SIZE)
    }

    /// `TempPyVectorInit` for the oscillator: the type object, its dispatch
    /// entry, and the factory into the `Math` module dictionary.
    pub(super) fn register() -> Result<PyRef, binding::BindError> {
        let type_object = binding::create_type(&spec())?;
        TYPE_ADDR.store(type_object.as_ptr() as usize, Ordering::SeqCst);
        v4provider::register_output(type_addr(), lfo_output);

        // SAFETY: `register` runs from interpreter init; the interpreter is up.
        unsafe {
            let module = capi::PyImport_AddModule(c"Math".as_ptr());
            if !module.is_null() {
                let dict = capi::PyModule_GetDict(module);
                let factory = capi::PyCFunction_New(factory_def(), std::ptr::null_mut());
                if !factory.is_null() {
                    capi::PyDict_SetItemString(dict, c"Vector4LFO".as_ptr(), factory);
                    capi::dec_ref(factory);
                }
            }
        }
        Ok(type_object)
    }
}

/// The `Math.Vector4MatrixAdaptor` provider
/// (`script_math.cpp:4308-4510`): takes a `Vector4Provider` and elevates it
/// to a `MatrixProvider`, under a `style` that says how the four components
/// read -- a scale, an axis rotation, or a look-at basis -- with an optional
/// position provider for the translation.
///
/// The reference's `matrix()` leaves the out argument untouched when the
/// source slot is empty (`:4435-4437`); the port's out argument is the
/// `Matrix::zero()` the reference's callers default it to, so an empty
/// source is the zero matrix, and a `None` position provider is the zero
/// translation. The two provider slots are the reference's
/// `Vector4ProviderPtr` members, whose `setData` coerces a `Vector4` or a
/// tuple into a fresh `Vector4Basic`.
mod v4adaptor {
    use super::*;
    use foundation::math::Matrix;
    use foundation::math::Vector3;
    use foundation::math::Vector4;

    /// The `Style` names in enum order (`script_math.cpp:4322-4336`): the
    /// map's index is the enum value, `STYLE_XYZ_SCALE` first.
    const STYLES: [&str; 7] = [
        "XYZ_SCALE",
        "XY_SCALE",
        "X_ROTATE",
        "Y_ROTATE",
        "Z_ROTATE",
        "LOOKAT",
        "LOOKAT_SCALEZ",
    ];

    /// The instance body beyond the `PyObject` header: the style, then the
    /// two held `Vector4Provider`s, the layout of the reference's
    /// `Vector4MatrixAdaptor` minus the vtable.
    #[repr(C)]
    struct AdaptorBody {
        header: capi::PyObject,
        style: u8,
        source: *mut capi::PyObject,
        position: *mut capi::PyObject,
    }

    const SOURCE: usize = std::mem::offset_of!(AdaptorBody, source);
    const POSITION: usize = std::mem::offset_of!(AdaptorBody, position);

    const INSTANCE_SIZE: usize =
        std::mem::size_of::<AdaptorBody>() - std::mem::size_of::<capi::PyObject>();

    /// The one `Math.Vector4MatrixAdaptor` type object, by address.
    static TYPE_ADDR: AtomicUsize = AtomicUsize::new(0);

    fn type_addr() -> usize {
        TYPE_ADDR.load(Ordering::SeqCst)
    }

    /// `Vector4MatrixAdaptor::matrix` (`script_math.cpp:4434-4509`): the
    /// source becomes a scale, an axis rotation or a look-at basis, the
    /// position provider the translation.
    fn adaptor_matrix(object: *mut capi::PyObject) -> Matrix {
        let mut m = Matrix::zero();
        // `if ( !source_ ) return;`: the out argument is left untouched.
        let Some(source) =
            v4provider::output_of(unsafe { v4provider::slot_read::<SOURCE>(object) })
        else {
            return m;
        };
        let v = Vector4::new(source[0], source[1], source[2], source[3]);

        // `Vector4 tr( Vector4::zero() );` then the position provider, when
        // held, overwrites it.
        let tr = v4provider::output_of(unsafe { v4provider::slot_read::<POSITION>(object) })
            .unwrap_or([0.0, 0.0, 0.0, 0.0]);
        let pos = Vector3::new(tr[0], tr[1], tr[2]);

        // SAFETY: the receiver is an `AdaptorBody`.
        let style = unsafe { std::ptr::addr_of!((*object.cast::<AdaptorBody>()).style).read() };
        match style {
            // STYLE_XYZ_SCALE
            0 => {
                m.set_scale(v.x, v.y, v.z);
                m.translation(&pos);
            }
            // STYLE_XY_SCALE
            1 => {
                m.set_scale(v.x, v.y, 1.0);
                m.translation(&pos);
            }
            // STYLE_X_ROTATE
            2 => {
                m.set_rotate_x(v.x);
                m.translation(&pos);
            }
            // STYLE_Y_ROTATE
            3 => {
                m.set_rotate_y(v.y);
                m.translation(&pos);
            }
            // STYLE_Z_ROTATE
            4 => {
                m.set_rotate_z(v.z);
                m.translation(&pos);
            }
            // STYLE_LOOKAT
            5 => {
                let at = Vector3::new(v.x, v.y, v.z);
                let mut up = Vector3::new(0.0, 1.0, 0.0);
                if up.dot_product(&at) > 0.95 {
                    up = Vector3::new(0.0, 0.0, 1.0);
                }
                m.look_at(&pos, &at, &up);
                let _ = m.invert();
            }
            // STYLE_LOOKAT_SCALEZ
            6 => {
                let at = Vector3::new(v.x, v.y, v.z);
                let mut up = Vector3::new(0.0, 1.0, 0.0);
                if up.dot_product(&at) > 0.95 {
                    up = Vector3::new(0.0, 0.0, 1.0);
                }
                let origin = Vector3::new(0.0, 0.0, 0.0);
                m.look_at(&origin, &at, &up);
                let _ = m.invert();
                let scale = v.length();
                let mut sc = Matrix::zero();
                sc.set_scale(1.0, 1.0, scale);
                m.pre_multiply(&sc);
                m.translation(&pos);
            }
            // A style value outside the enum leaves the reference's switch
            // with `m` as the caller's default.
            _ => {}
        }
        m
    }

    /// `Vector4MatrixAdaptor::pyGet_style` (the enum `getData` at
    /// `script.hpp:872-886`): the style's name.
    unsafe extern "C" fn get_style(
        self_: *mut capi::PyObject,
        _closure: *mut std::os::raw::c_void,
    ) -> *mut capi::PyObject {
        // SAFETY: the receiver is an `AdaptorBody`.
        let index = unsafe { std::ptr::addr_of!((*self_.cast::<AdaptorBody>()).style).read() };
        match STYLES.get(index as usize) {
            Some(name) => unsafe { capi::unicode_from_bytes(name.as_bytes()) },
            None => capi::py_none(),
        }
    }

    /// `Vector4MatrixAdaptor::pySet_style` (the enum `setData` at
    /// `script.hpp:830-869`): one of the seven names, `ValueError` for a
    /// string that names no style, `TypeError` for anything else.
    unsafe extern "C" fn set_style(
        self_: *mut capi::PyObject,
        value: *mut capi::PyObject,
        _closure: *mut std::os::raw::c_void,
    ) -> c_int {
        let is_string = unsafe { capi::py_unicode_check(value) };
        if is_string {
            let name = unsafe { capi::to_string(value) };
            if let Some(index) = STYLES.iter().position(|style| *style == name) {
                // SAFETY: the receiver is an `AdaptorBody`.
                unsafe {
                    std::ptr::addr_of_mut!((*self_.cast::<AdaptorBody>()).style).write(index as u8);
                }
                return 0;
            }
        }
        // `PyErr_SetString( PyUnicode_Check(...) ? ValueError : TypeError )`.
        let message = "style must be set to one of XYZ_SCALE, XY_SCALE, X_ROTATE, Y_ROTATE, Z_ROTATE, \
             LOOKAT, or LOOKAT_SCALEZ";
        if is_string {
            value_error(message);
        } else {
            type_error(message);
        }
        -1
    }

    /// A fresh `Math.Vector4MatrixAdaptor`; the caller owns the reference.
    /// The constructor's field values are the reference's (`:4345-4350`):
    /// the `XY_SCALE` style and two empty provider slots.
    fn make() -> *mut capi::PyObject {
        let Ok(type_object) = binding::create_type(&spec()) else {
            return std::ptr::null_mut();
        };
        let Some(instance) = (unsafe { binding::instantiate(&type_object) }) else {
            return std::ptr::null_mut();
        };
        let pointer = instance.as_ptr();
        // SAFETY: the allocated body is exactly an `AdaptorBody`.
        unsafe {
            std::ptr::addr_of_mut!((*pointer.cast::<AdaptorBody>()).style).write(1);
            std::ptr::addr_of_mut!((*pointer.cast::<AdaptorBody>()).source)
                .write(std::ptr::null_mut());
            std::ptr::addr_of_mut!((*pointer.cast::<AdaptorBody>()).position)
                .write(std::ptr::null_mut());
        }
        std::mem::forget(instance);
        pointer
    }

    /// The `PY_AUTO_FACTORY_DECLARE( Vector4MatrixAdaptor, END )` factory: a
    /// fresh adaptor, the reference's `END` argument list meaning no
    /// arguments.
    unsafe extern "C" fn factory(
        _module: *mut capi::PyObject,
        args: *mut capi::PyObject,
    ) -> *mut capi::PyObject {
        if !want_args(args, 0) {
            return type_error("Math.Vector4MatrixAdaptor() takes no arguments");
        }
        make()
    }

    /// The factory descriptor for `PyCFunction_New`; leaked for the type's
    /// lifetime, the reference's static method table.
    fn factory_def() -> *mut capi::PyMethodDef {
        Box::leak(Box::new(capi::PyMethodDef {
            ml_name: c"Vector4MatrixAdaptor".as_ptr() as *const c_char,
            ml_meth: Some(factory),
            ml_flags: capi::METH_VARARGS,
            ml_doc: std::ptr::null(),
        }))
    }

    /// The `PY_BEGIN_ATTRIBUTES` table of `Vector4MatrixAdaptor`: the two
    /// provider slots and the style, the reference's
    /// `PY_RW_ATTRIBUTE_DECLARE` converter pairs.
    fn spec() -> binding::TypeSpec {
        binding::TypeSpec::new("Math.Vector4MatrixAdaptor")
            .base(matrix::provider_addr())
            .attribute_with(
                "source",
                v4provider::slot_get::<SOURCE>,
                Some(v4provider::slot_set::<SOURCE>),
                c"source".as_ptr() as *mut std::os::raw::c_void,
            )
            .attribute_with(
                "position",
                v4provider::slot_get::<POSITION>,
                Some(v4provider::slot_set::<POSITION>),
                c"position".as_ptr() as *mut std::os::raw::c_void,
            )
            .attribute("style", get_style, Some(set_style))
            .dealloc(dealloc)
            .instance_size(INSTANCE_SIZE)
    }

    provider_dealloc!(dealloc, AdaptorBody, source, position);

    /// `TempPyVectorInit` for the adaptor: the type object, its `matrix()`
    /// dispatch entry, and the factory into the `Math` module dictionary.
    pub(super) fn register() -> Result<PyRef, binding::BindError> {
        let type_object = binding::create_type(&spec())?;
        TYPE_ADDR.store(type_object.as_ptr() as usize, Ordering::SeqCst);
        matrix::register_matrix_output(type_addr(), adaptor_matrix);

        // SAFETY: `register` runs from interpreter init; the interpreter is up.
        unsafe {
            let module = capi::PyImport_AddModule(c"Math".as_ptr());
            if !module.is_null() {
                let dict = capi::PyModule_GetDict(module);
                let factory = capi::PyCFunction_New(factory_def(), std::ptr::null_mut());
                if !factory.is_null() {
                    capi::PyDict_SetItemString(dict, c"Vector4MatrixAdaptor".as_ptr(), factory);
                    capi::dec_ref(factory);
                }
            }
        }
        Ok(type_object)
    }
}

/// The `Math.Vector4Swizzle` provider (`script_math.cpp:4511-4641`).
///
/// Four held `Vector4Provider`s, one per output component: each slot's
/// provider contributes its own `x` component to the matching component of
/// the output.
mod v4swizzle {
    use super::*;

    /// The instance body beyond the `PyObject` header: the four held
    /// `Vector4Provider`s, the layout of the reference's `Vector4Swizzle`.
    #[repr(C)]
    struct SwizzleBody {
        header: capi::PyObject,
        x: *mut capi::PyObject,
        y: *mut capi::PyObject,
        z: *mut capi::PyObject,
        w: *mut capi::PyObject,
    }

    const X: usize = std::mem::offset_of!(SwizzleBody, x);
    const Y: usize = std::mem::offset_of!(SwizzleBody, y);
    const Z: usize = std::mem::offset_of!(SwizzleBody, z);
    const W: usize = std::mem::offset_of!(SwizzleBody, w);

    const INSTANCE_SIZE: usize =
        std::mem::size_of::<SwizzleBody>() - std::mem::size_of::<capi::PyObject>();

    /// The one `Math.Vector4Swizzle` type object, by address.
    static TYPE_ADDR: AtomicUsize = AtomicUsize::new(0);

    fn type_addr() -> usize {
        TYPE_ADDR.load(Ordering::SeqCst)
    }

    /// `Vector4Swizzle::output` (`script_math.cpp:4605-4628`): each held
    /// provider's `x` into the matching component. An empty slot leaves the
    /// caller's component untouched, which `Vector4 val;` defaults to zero.
    fn swizzle_output(object: *mut capi::PyObject) -> [f32; 4] {
        let mut out = [0.0f32; 4];
        if let Some(values) = v4provider::output_of(unsafe { v4provider::slot_read::<X>(object) }) {
            out[0] = values[0];
        }
        if let Some(values) = v4provider::output_of(unsafe { v4provider::slot_read::<Y>(object) }) {
            out[1] = values[0];
        }
        if let Some(values) = v4provider::output_of(unsafe { v4provider::slot_read::<Z>(object) }) {
            out[2] = values[0];
        }
        if let Some(values) = v4provider::output_of(unsafe { v4provider::slot_read::<W>(object) }) {
            out[3] = values[0];
        }
        out
    }

    /// A fresh `Math.Vector4Swizzle`; the caller owns the reference. All four
    /// slots are empty, the reference's constructor initialisers.
    fn make() -> *mut capi::PyObject {
        let Ok(type_object) = binding::create_type(&spec()) else {
            return std::ptr::null_mut();
        };
        let Some(instance) = (unsafe { binding::instantiate(&type_object) }) else {
            return std::ptr::null_mut();
        };
        let pointer = instance.as_ptr();
        // SAFETY: the allocated body is exactly a `SwizzleBody`.
        unsafe {
            std::ptr::addr_of_mut!((*pointer.cast::<SwizzleBody>()).x).write(std::ptr::null_mut());
            std::ptr::addr_of_mut!((*pointer.cast::<SwizzleBody>()).y).write(std::ptr::null_mut());
            std::ptr::addr_of_mut!((*pointer.cast::<SwizzleBody>()).z).write(std::ptr::null_mut());
            std::ptr::addr_of_mut!((*pointer.cast::<SwizzleBody>()).w).write(std::ptr::null_mut());
        };
        std::mem::forget(instance);
        pointer
    }

    /// `Math.Vector4Swizzle` with no arguments (`PY_AUTO_FACTORY_DECLARE`
    /// with `END`): a swizzle with all four slots empty.
    unsafe extern "C" fn factory(
        _module: *mut capi::PyObject,
        args: *mut capi::PyObject,
    ) -> *mut capi::PyObject {
        if !want_args(args, 0) {
            return type_error("Math.Vector4Swizzle() takes no arguments");
        }
        make()
    }

    /// The factory descriptor for `PyCFunction_New`; leaked for the type's
    /// lifetime, the reference's static method table.
    fn factory_def() -> *mut capi::PyMethodDef {
        Box::leak(Box::new(capi::PyMethodDef {
            ml_name: c"Vector4Swizzle".as_ptr() as *const c_char,
            ml_meth: Some(factory),
            ml_flags: capi::METH_VARARGS,
            ml_doc: std::ptr::null(),
        }))
    }

    /// The `PY_BEGIN_ATTRIBUTES` table of `Vector4Swizzle` and its
    /// destruction entry: the four held providers, the reference's
    /// `PY_RW_ATTRIBUTE_DECLARE` converter pairs.
    fn spec() -> binding::TypeSpec {
        binding::TypeSpec::new("Math.Vector4Swizzle")
            .base(v4provider::base_addr())
            .attribute_with(
                "x",
                v4provider::slot_get::<X>,
                Some(v4provider::slot_set::<X>),
                c"x".as_ptr() as *mut std::os::raw::c_void,
            )
            .attribute_with(
                "y",
                v4provider::slot_get::<Y>,
                Some(v4provider::slot_set::<Y>),
                c"y".as_ptr() as *mut std::os::raw::c_void,
            )
            .attribute_with(
                "z",
                v4provider::slot_get::<Z>,
                Some(v4provider::slot_set::<Z>),
                c"z".as_ptr() as *mut std::os::raw::c_void,
            )
            .attribute_with(
                "w",
                v4provider::slot_get::<W>,
                Some(v4provider::slot_set::<W>),
                c"w".as_ptr() as *mut std::os::raw::c_void,
            )
            .dealloc(dealloc)
            .instance_size(INSTANCE_SIZE)
    }

    provider_dealloc!(dealloc, SwizzleBody, x, y, z, w);

    /// `TempPyVectorInit` for the swizzle: the type object, its `output`
    /// dispatch entry, and the factory into the `Math` module dictionary.
    pub(super) fn register() -> Result<PyRef, binding::BindError> {
        let type_object = binding::create_type(&spec())?;
        TYPE_ADDR.store(type_object.as_ptr() as usize, Ordering::SeqCst);
        v4provider::register_output(type_addr(), swizzle_output);

        // SAFETY: `register` runs from interpreter init; the interpreter is up.
        unsafe {
            let module = capi::PyImport_AddModule(c"Math".as_ptr());
            if !module.is_null() {
                let dict = capi::PyModule_GetDict(module);
                let factory = capi::PyCFunction_New(factory_def(), std::ptr::null_mut());
                if !factory.is_null() {
                    capi::PyDict_SetItemString(dict, c"Vector4Swizzle".as_ptr(), factory);
                    capi::dec_ref(factory);
                }
            }
        }
        Ok(type_object)
    }
}

/// The `Math.Vector4Combiner` provider (`script_math.cpp:4643-4811`).
///
/// Two held `Vector4Provider`s combined componentwise by one of seven named
/// functions; the `ADD` function is the constructor default.
mod v4combiner {
    use super::*;

    /// The `Fn` names in enum order (`script_math.cpp:4653-4676`): the map's
    /// index is the enum value, `FN_MULTIPLY` first.
    const FNS: [&str; 7] = ["MULTIPLY", "DIVIDE", "ADD", "SUBTRACT", "DOT", "MIN", "MAX"];

    /// The instance body beyond the `PyObject` header: the function, then the
    /// two held `Vector4Provider`s, the layout of the reference's
    /// `Vector4Combiner`.
    #[repr(C)]
    struct CombinerBody {
        header: capi::PyObject,
        fn_: u8,
        a: *mut capi::PyObject,
        b: *mut capi::PyObject,
    }

    const A: usize = std::mem::offset_of!(CombinerBody, a);
    const B: usize = std::mem::offset_of!(CombinerBody, b);

    const INSTANCE_SIZE: usize =
        std::mem::size_of::<CombinerBody>() - std::mem::size_of::<capi::PyObject>();

    /// The one `Math.Vector4Combiner` type object, by address.
    static TYPE_ADDR: AtomicUsize = AtomicUsize::new(0);

    fn type_addr() -> usize {
        TYPE_ADDR.load(Ordering::SeqCst)
    }

    /// `Vector4Combiner::output` (`script_math.cpp:4770-4811`): `a` combined
    /// with `b` by the selected function. Either slot empty leaves the
    /// caller's vector, the zero default.
    fn combiner_output(object: *mut capi::PyObject) -> [f32; 4] {
        let a_values = v4provider::output_of(unsafe { v4provider::slot_read::<A>(object) });
        let b_values = v4provider::output_of(unsafe { v4provider::slot_read::<B>(object) });
        let (Some(mut v), Some(b)) = (a_values, b_values) else {
            return [0.0; 4];
        };
        // SAFETY: the receiver is a `CombinerBody`.
        let function = unsafe { std::ptr::addr_of!((*object.cast::<CombinerBody>()).fn_).read() };
        match function {
            // FN_MULTIPLY: the componentwise product.
            0 => {
                v[0] *= b[0];
                v[1] *= b[1];
                v[2] *= b[2];
                v[3] *= b[3];
            }
            // FN_DIVIDE: the componentwise quotient.
            1 => {
                v[0] /= b[0];
                v[1] /= b[1];
                v[2] /= b[2];
                v[3] /= b[3];
            }
            // FN_ADD.
            2 => {
                v[0] += b[0];
                v[1] += b[1];
                v[2] += b[2];
                v[3] += b[3];
            }
            // FN_SUBTRACT.
            3 => {
                v[0] -= b[0];
                v[1] -= b[1];
                v[2] -= b[2];
                v[3] -= b[3];
            }
            // FN_DOT: the four-term dot, set into all components.
            4 => {
                let s = v[0] * b[0] + v[1] * b[1] + v[2] * b[2] + v[3] * b[3];
                v = [s, s, s, s];
            }
            // FN_MIN: `std::min( v, b )` per component, `b` on ties.
            5 => {
                v[0] = if b[0] < v[0] { b[0] } else { v[0] };
                v[1] = if b[1] < v[1] { b[1] } else { v[1] };
                v[2] = if b[2] < v[2] { b[2] } else { v[2] };
                v[3] = if b[3] < v[3] { b[3] } else { v[3] };
            }
            // FN_MAX: `std::max( v, b )` per component, `v` on ties.
            6 => {
                v[0] = if v[0] < b[0] { b[0] } else { v[0] };
                v[1] = if v[1] < b[1] { b[1] } else { v[1] };
                v[2] = if v[2] < b[2] { b[2] } else { v[2] };
                v[3] = if v[3] < b[3] { b[3] } else { v[3] };
            }
            // A function value outside the enum leaves the reference's
            // switch with `v` as `a`'s output.
            _ => {}
        }
        v
    }

    /// `Vector4Combiner::pyGet_fn` (the enum `getData` at
    /// `script.hpp:872-886`): the function's name.
    unsafe extern "C" fn get_fn(
        self_: *mut capi::PyObject,
        _closure: *mut std::os::raw::c_void,
    ) -> *mut capi::PyObject {
        // SAFETY: the receiver is a `CombinerBody`.
        let index = unsafe { std::ptr::addr_of!((*self_.cast::<CombinerBody>()).fn_).read() };
        match FNS.get(index as usize) {
            Some(name) => unsafe { capi::unicode_from_bytes(name.as_bytes()) },
            // The out-of-range arm the reference's converter leaves for a
            // corrupted enum value.
            None => capi::py_none(),
        }
    }

    /// `Vector4Combiner::pySet_fn` (the enum `setData` at
    /// `script.hpp:830-869`): one of the seven names, `ValueError` for a
    /// string that names no function, `TypeError` for anything else.
    unsafe extern "C" fn set_fn(
        self_: *mut capi::PyObject,
        value: *mut capi::PyObject,
        _closure: *mut std::os::raw::c_void,
    ) -> c_int {
        let is_string = unsafe { capi::py_unicode_check(value) };
        if is_string {
            let name = unsafe { capi::to_string(value) };
            if let Some(index) = FNS.iter().position(|function| *function == name) {
                // SAFETY: the receiver is a `CombinerBody`.
                unsafe {
                    std::ptr::addr_of_mut!((*self_.cast::<CombinerBody>()).fn_).write(index as u8);
                }
                return 0;
            }
        }
        // `PyErr_SetString( PyUnicode_Check(...) ? ValueError : TypeError )`.
        let message = "fn must be set to one of MULTIPLY, DIVIDE, ADD, SUBTRACT, DOT, MIN, or MAX";
        if is_string {
            value_error(message);
        } else {
            type_error(message);
        }
        -1
    }

    /// A fresh `Math.Vector4Combiner`; the caller owns the reference. The
    /// constructor's field values are the reference's (`:4681-4687`): the
    /// `FN_ADD` function and two empty provider slots.
    fn make() -> *mut capi::PyObject {
        let Ok(type_object) = binding::create_type(&spec()) else {
            return std::ptr::null_mut();
        };
        let Some(instance) = (unsafe { binding::instantiate(&type_object) }) else {
            return std::ptr::null_mut();
        };
        let pointer = instance.as_ptr();
        // SAFETY: the allocated body is exactly a `CombinerBody`.
        unsafe {
            std::ptr::addr_of_mut!((*pointer.cast::<CombinerBody>()).fn_).write(2u8);
            std::ptr::addr_of_mut!((*pointer.cast::<CombinerBody>()).a).write(std::ptr::null_mut());
            std::ptr::addr_of_mut!((*pointer.cast::<CombinerBody>()).b).write(std::ptr::null_mut());
        };
        std::mem::forget(instance);
        pointer
    }

    /// `Math.Vector4Combiner` with no arguments (`PY_AUTO_FACTORY_DECLARE`
    /// with `END`): a combiner with the `ADD` function and two empty slots.
    unsafe extern "C" fn factory(
        _module: *mut capi::PyObject,
        args: *mut capi::PyObject,
    ) -> *mut capi::PyObject {
        if !want_args(args, 0) {
            return type_error("Math.Vector4Combiner() takes no arguments");
        }
        make()
    }

    /// The factory descriptor for `PyCFunction_New`; leaked for the type's
    /// lifetime, the reference's static method table.
    fn factory_def() -> *mut capi::PyMethodDef {
        Box::leak(Box::new(capi::PyMethodDef {
            ml_name: c"Vector4Combiner".as_ptr() as *const c_char,
            ml_meth: Some(factory),
            ml_flags: capi::METH_VARARGS,
            ml_doc: std::ptr::null(),
        }))
    }

    /// The `PY_BEGIN_ATTRIBUTES` table of `Vector4Combiner`: the function
    /// and the two provider slots, the reference's `PY_DEFERRED_ATTRIBUTE`
    /// and `PY_RW_ATTRIBUTE_DECLARE` converter pairs.
    fn spec() -> binding::TypeSpec {
        binding::TypeSpec::new("Math.Vector4Combiner")
            .base(v4provider::base_addr())
            .attribute("fn", get_fn, Some(set_fn))
            .attribute_with(
                "a",
                v4provider::slot_get::<A>,
                Some(v4provider::slot_set::<A>),
                c"a".as_ptr() as *mut std::os::raw::c_void,
            )
            .attribute_with(
                "b",
                v4provider::slot_get::<B>,
                Some(v4provider::slot_set::<B>),
                c"b".as_ptr() as *mut std::os::raw::c_void,
            )
            .dealloc(dealloc)
            .instance_size(INSTANCE_SIZE)
    }

    provider_dealloc!(dealloc, CombinerBody, a, b);

    /// `TempPyVectorInit` for the combiner: the type object, its `output`
    /// dispatch entry, and the factory into the `Math` module dictionary.
    pub(super) fn register() -> Result<PyRef, binding::BindError> {
        let type_object = binding::create_type(&spec())?;
        TYPE_ADDR.store(type_object.as_ptr() as usize, Ordering::SeqCst);
        v4provider::register_output(type_addr(), combiner_output);

        // SAFETY: `register` runs from interpreter init; the interpreter is up.
        unsafe {
            let module = capi::PyImport_AddModule(c"Math".as_ptr());
            if !module.is_null() {
                let dict = capi::PyModule_GetDict(module);
                let factory = capi::PyCFunction_New(factory_def(), std::ptr::null_mut());
                if !factory.is_null() {
                    capi::PyDict_SetItemString(dict, c"Vector4Combiner".as_ptr(), factory);
                    capi::dec_ref(factory);
                }
            }
        }
        Ok(type_object)
    }
}

/// The `Math.Vector4Shader` provider and the `Math.Vector4Register`
/// scratch registers it reads (`script_math.cpp:4826-5320`).
///
/// A shader is a small program: `addOp` appends one instruction (opcode,
/// output register, two inputs) and `output` runs the list into a register
/// file every shader shares. The file is the reference's static `r_[128]`:
/// `r_[0]` the final output, `r_[0..63]` the temporary registers the
/// `Math.getRegister` providers address, `r_[64..]` the user registers each
/// unique input is assigned once per pass. Nothing clears the file between
/// passes, so a fresh shader reads whatever `r_[0]` held last -- the
/// reference's never-reset statics are the port's statics too.
///
/// The thirteen opcodes run in the reference's switch order
/// (`:5136-5236`), including its `OP_SGE`/`OP_SLT` compare of `i0.x`
/// against all four `i1` lanes: the original reads the scalar into every
/// lane, and the port keeps the result, so a script sees the reference
/// arithmetic.
mod v4shader {
    use super::*;
    use std::sync::Mutex;

    /// The reference's magic numbers (`script_math.cpp:4846-4850`).
    const MAX_TEMP_REGISTERS: usize = 63;
    const NUM_USER_REGISTERS: usize = 64;
    /// The register number of a missing provider. It is also a live file
    /// index: a null input reads `r_[63]`, a refused output register writes
    /// there, the reference's `UNUSED_REGISTER`.
    const UNUSED_REGISTER: u8 = 63;
    const BASE_USER_REGISTER: usize = 64;
    const NUM_REGISTERS: usize = 128;

    /// The shared register file, the reference's static
    /// `Vector4 Vector4Shader::r_[NUM_REGISTERS]` (`:5017`).
    static REGISTERS: Mutex<[[f32; 4]; NUM_REGISTERS]> = Mutex::new([[0.0; 4]; NUM_REGISTERS]);

    /// The temporary register providers, the reference's static
    /// `Vector4ProviderPtr Vector4Shader::pr_[MAX_TEMP_REGISTERS]`
    /// (`:5016`): empty until the first `Math.getRegister` call creates all
    /// sixty-three, and held forever after, the reference's "smartpointers
    /// default to NULL" note plus its never-released statics. Addresses sit
    /// as `usize`, the static-storage rule that keeps the state `Send`.
    static TEMP_PROVIDERS: Mutex<Vec<usize>> = Mutex::new(Vec::new());

    /// One temporary register's value, `Vector4Register::output` reading its
    /// `value_` (`:4827`).
    pub(super) fn register_read(index: u8) -> [f32; 4] {
        REGISTERS
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())[index as usize]
    }

    /// One instruction: the opcode and its three register numbers
    /// (`script_math.hpp`'s `Vector4Shader::Instruction`). The output is
    /// always a temporary register; an input may be one, a user register, or
    /// [`UNUSED_REGISTER`].
    #[derive(Clone, Copy)]
    struct Instruction {
        op: u8,
        out: u8,
        in0: u8,
        in1: u8,
    }

    /// The instance body beyond the `PyObject` header: the instruction list
    /// and the unique input providers, the layout of the reference's
    /// `instructions_` and `uniqueInputs_`.
    #[repr(C)]
    struct ShaderBody {
        header: capi::PyObject,
        instructions: Vec<Instruction>,
        unique_inputs: Vec<*mut capi::PyObject>,
    }

    const INSTANCE_SIZE: usize =
        std::mem::size_of::<ShaderBody>() - std::mem::size_of::<capi::PyObject>();

    /// The one `Math.Vector4Shader` type object, by address.
    static TYPE_ADDR: AtomicUsize = AtomicUsize::new(0);

    fn type_addr() -> usize {
        TYPE_ADDR.load(Ordering::SeqCst)
    }

    /// `ERROR_MSG` (`:5070`, `:5088`): the two refusal paths of
    /// `addInputVector` report and carry on, the reference's console noise
    /// with the instruction still appended.
    fn shader_log(message: &str) {
        eprintln!("{message}");
    }

    /// Release an owned reference the conversion handed over; a null means
    /// the conversion produced no object.
    unsafe fn drop_owned(pointer: *mut capi::PyObject) {
        if !pointer.is_null() {
            unsafe { capi::dec_ref(pointer) };
        }
    }

    /// `Script::setData( pObject, Vector4ProviderPtr&, varName )`
    /// (`script.hpp:610-631`) for one `addOp` argument: `coerce` the object,
    /// `None` the empty provider, a `Vector4Provider` held, anything else the
    /// converter's own `TypeError`. The returned pointer owns one reference,
    /// or is null for `None`.
    unsafe fn as_provider(
        object: *mut capi::PyObject,
        var_name: &str,
    ) -> Result<*mut capi::PyObject, ()> {
        let coerced = v4provider::coerce(object);
        if !coerced.is_null() && coerced == capi::py_none() {
            unsafe { capi::dec_ref(coerced) };
            return Ok(std::ptr::null_mut());
        }
        if v4provider::provider_check(coerced) {
            return Ok(coerced);
        }
        unsafe { drop_owned(coerced) };
        type_error(&format!(
            "{var_name} must be set to a Vector4Provider or None"
        ));
        Err(())
    }

    /// `Script::setData( pObject, long&, varName )` (`script.cpp:1693-1741`):
    /// a long read as-is, a float truncating, anything else the reference's
    /// own refusal.
    unsafe fn as_script_long(object: *mut capi::PyObject, var_name: &str) -> Option<i64> {
        if object.is_null() {
            return None;
        }
        // SAFETY: the read is on the borrowed argument; a failed conversion
        // leaves the error the caller's `None` arm replaces.
        unsafe {
            if capi::py_long_check(object) {
                // LP64: a C long is the 64-bit width, so `PyLong_AsLong` is
                // the reference's `PyLong_AsLongLong`.
                let value = capi::PyLong_AsLong(object);
                if !capi::PyErr_Occurred().is_null() {
                    capi::PyErr_Clear();
                    type_error(&format!("{var_name} must be set to a long"));
                    return None;
                }
                return Some(value as i64);
            }
            if capi::py_float_check(object) {
                return Some(capi::PyFloat_AsDouble(object) as i64);
            }
        }
        type_error(&format!("{var_name} must be set to a long"));
        None
    }

    /// `Script::setData( pObject, uint8&, varName )` (`script.hpp:208-234`,
    /// the Linux `INT_ACCESSOR( uint8, long )`): the long conversion, then
    /// the round-trip through `uint8` whose failure is the accessor's own
    /// "Integer is out of range".
    unsafe fn as_uint8(object: *mut capi::PyObject, var_name: &str) -> Option<u8> {
        let value = unsafe { as_script_long(object, var_name)? };
        let narrowed = value as u8;
        if narrowed as i64 != value {
            type_error("Integer is out of range");
            return None;
        }
        Some(narrowed)
    }

    /// `Vector4Shader::addInputVector` (`:5059-5100`): the register number
    /// of one input. A temporary register answers its own index; a new
    /// input is appended and given the next user register; the `must_be_temp`
    /// output path refuses a non-register with the reference's `ERROR_MSG`;
    /// the sixty-fifth unique input is refused too. The second return is
    /// whether the vector took over the caller's owned reference.
    fn add_input(
        shader: &mut ShaderBody,
        provider: *mut capi::PyObject,
        must_be_temp: bool,
    ) -> (u8, bool) {
        // No pointer, no register.
        if provider.is_null() {
            return (UNUSED_REGISTER, false);
        }
        // A temporary register answers its own number.
        if v4register::check(provider) {
            return (unsafe { v4register::index_of(provider) }, false);
        }
        if must_be_temp {
            shader_log("Vector4Shader::addInputVector - register that must be temporary was not");
            return (UNUSED_REGISTER, false);
        }
        if let Some(found) = shader
            .unique_inputs
            .iter()
            .position(|&stored| stored == provider)
        {
            return ((found + BASE_USER_REGISTER) as u8, false);
        }
        if shader.unique_inputs.len() == NUM_USER_REGISTERS {
            shader_log(
                "Vector4Shader::addInputVector - shader has exceeded \
                       the maximum number of user registers",
            );
            return (UNUSED_REGISTER, false);
        }
        shader.unique_inputs.push(provider);
        (
            (shader.unique_inputs.len() - 1 + BASE_USER_REGISTER) as u8,
            true,
        )
    }

    /// The componentwise `std::min`/`std::max` of the reference's switch:
    /// `min` answers `b` on a tie, `max` answers `a`.
    fn std_min(a: f32, b: f32) -> f32 {
        if b < a { b } else { a }
    }

    fn std_max(a: f32, b: f32) -> f32 {
        if a < b { b } else { a }
    }

    /// One opcode against the register file, the reference's switch
    /// (`:5136-5236`). The inputs are read into locals first, so an
    /// instruction writing the register it reads stays the reference's
    /// read-then-assign.
    fn run_instruction(registers: &mut [[f32; 4]; NUM_REGISTERS], ins: &Instruction) {
        let i0 = registers[ins.in0 as usize];
        let i1 = registers[ins.in1 as usize];
        let out = &mut registers[ins.out as usize];
        match ins.op {
            // OP_MOVE.
            0 => *out = i0,
            // OP_RECIPROCAL.
            1 => {
                out[0] = 1.0 / i0[0];
                out[1] = 1.0 / i0[1];
                out[2] = 1.0 / i0[2];
                out[3] = 1.0 / i0[3];
            }
            // OP_BIAS.
            2 => {
                out[0] = i0[0] - 0.5;
                out[1] = i0[1] - 0.5;
                out[2] = i0[2] - 0.5;
                out[3] = i0[3] - 0.5;
            }
            // OP_COMPLEMENT.
            3 => {
                out[0] = 1.0 - i0[0];
                out[1] = 1.0 - i0[1];
                out[2] = 1.0 - i0[2];
                out[3] = 1.0 - i0[3];
            }
            // OP_MULTIPLY.
            4 => {
                out[0] = i0[0] * i1[0];
                out[1] = i0[1] * i1[1];
                out[2] = i0[2] * i1[2];
                out[3] = i0[3] * i1[3];
            }
            // OP_DIVIDE.
            5 => {
                out[0] = i0[0] / i1[0];
                out[1] = i0[1] / i1[1];
                out[2] = i0[2] / i1[2];
                out[3] = i0[3] / i1[3];
            }
            // OP_ADD.
            6 => {
                out[0] = i0[0] + i1[0];
                out[1] = i0[1] + i1[1];
                out[2] = i0[2] + i1[2];
                out[3] = i0[3] + i1[3];
            }
            // OP_SUBTRACT.
            7 => {
                out[0] = i0[0] - i1[0];
                out[1] = i0[1] - i1[1];
                out[2] = i0[2] - i1[2];
                out[3] = i0[3] - i1[3];
            }
            // OP_DOT: the four-term product sum spread into all components.
            8 => {
                let s = i0[0] * i1[0] + i0[1] * i1[1] + i0[2] * i1[2] + i0[3] * i1[3];
                *out = [s, s, s, s];
            }
            // OP_MIN.
            9 => {
                out[0] = std_min(i0[0], i1[0]);
                out[1] = std_min(i0[1], i1[1]);
                out[2] = std_min(i0[2], i1[2]);
                out[3] = std_min(i0[3], i1[3]);
            }
            // OP_MAX.
            10 => {
                out[0] = std_max(i0[0], i1[0]);
                out[1] = std_max(i0[1], i1[1]);
                out[2] = std_max(i0[2], i1[2]);
                out[3] = std_max(i0[3], i1[3]);
            }
            // OP_SGE: the reference compares `i0.x` against all four `i1`
            // lanes, the quirk `:5199-5203`.
            11 => {
                out[0] = (i0[0] >= i1[0]) as u8 as f32;
                out[1] = (i0[0] >= i1[1]) as u8 as f32;
                out[2] = (i0[0] >= i1[2]) as u8 as f32;
                out[3] = (i0[0] >= i1[3]) as u8 as f32;
            }
            // OP_SLT: the same scalar compare.
            12 => {
                out[0] = (i0[0] < i1[0]) as u8 as f32;
                out[1] = (i0[0] < i1[1]) as u8 as f32;
                out[2] = (i0[0] < i1[2]) as u8 as f32;
                out[3] = (i0[0] < i1[3]) as u8 as f32;
            }
            // An opcode outside the enum: the reference's switch has no
            // default, so the output register keeps its previous value.
            _ => {}
        }
    }

    /// `Vector4Shader::output` (`:5104-5246`): every unique input evaluated
    /// once into its user register, then the instruction list, then
    /// `finalOutput = r_[0]`. The unique inputs are evaluated outside the
    /// file lock, the reference's single pass through `uniqueInputs_` before
    /// `instructions_`.
    fn shader_output(object: *mut capi::PyObject) -> [f32; 4] {
        // SAFETY: the receiver is a `ShaderBody`.
        let body = unsafe { &*object.cast::<ShaderBody>() };
        let mut user_values = Vec::with_capacity(body.unique_inputs.len());
        for &input in &body.unique_inputs {
            user_values.push(
                v4provider::output_of(input)
                    .expect("a stored unique input is a registered Vector4Provider"),
            );
        }
        let mut registers = REGISTERS
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        for (offset, value) in user_values.iter().enumerate() {
            registers[BASE_USER_REGISTER + offset] = *value;
        }
        for instruction in &body.instructions {
            run_instruction(&mut registers, instruction);
        }
        registers[0]
    }

    /// A fresh `Math.Vector4Shader`; the caller owns the reference. No
    /// instructions, the reference's default constructor.
    fn make() -> *mut capi::PyObject {
        let Ok(type_object) = binding::create_type(&spec()) else {
            return std::ptr::null_mut();
        };
        let Some(instance) = (unsafe { binding::instantiate(&type_object) }) else {
            return std::ptr::null_mut();
        };
        let pointer = instance.as_ptr();
        // SAFETY: the allocated body is exactly a `ShaderBody`.
        unsafe {
            std::ptr::addr_of_mut!((*pointer.cast::<ShaderBody>()).instructions).write(Vec::new());
            std::ptr::addr_of_mut!((*pointer.cast::<ShaderBody>()).unique_inputs).write(Vec::new());
        };
        std::mem::forget(instance);
        pointer
    }

    /// `Math.Vector4Shader` with no arguments (`PY_AUTO_FACTORY_DECLARE`
    /// with `END`).
    unsafe extern "C" fn factory(
        _module: *mut capi::PyObject,
        args: *mut capi::PyObject,
    ) -> *mut capi::PyObject {
        if !want_args(args, 0) {
            return type_error("Math.Vector4Shader() takes no arguments");
        }
        make()
    }

    /// The factory descriptor for `PyCFunction_New`; leaked for the type's
    /// lifetime, the reference's static method table.
    fn factory_def() -> *mut capi::PyMethodDef {
        Box::leak(Box::new(capi::PyMethodDef {
            ml_name: c"Vector4Shader".as_ptr() as *const c_char,
            ml_meth: Some(factory),
            ml_flags: capi::METH_VARARGS,
            ml_doc: std::ptr::null(),
        }))
    }

    /// `Vector4Shader::addOp` (`:5101-5117`): one instruction appended, the
    /// arguments the reference's `ARG(uint8, ARG(Vector4ProviderPtr,
    /// ARG(Vector4ProviderPtr, OPTARG(Vector4ProviderPtr, NULL, END))))`.
    unsafe extern "C" fn add_op(
        self_: *mut capi::PyObject,
        args: *mut capi::PyObject,
    ) -> *mut capi::PyObject {
        // SAFETY: the count read is on the borrowed arguments tuple.
        let count = unsafe { capi::PyTuple_Size(args) };
        if !(3..=4).contains(&count) {
            return type_error(
                "addOp() expects between 3 and 4 arguments of types uint8, \
                 Vector4Provider, Vector4Provider, Vector4Provider",
            );
        }
        let Some(opcode) = (unsafe { as_uint8(arg(args, 0), "() argument 1") }) else {
            // The conversion left its own error.
            return std::ptr::null_mut();
        };
        let Ok(out_reg) = (unsafe { as_provider(arg(args, 1), "() argument 2") }) else {
            return std::ptr::null_mut();
        };
        let Ok(reg1) = (unsafe { as_provider(arg(args, 2), "() argument 3") }) else {
            unsafe { drop_owned(out_reg) };
            return std::ptr::null_mut();
        };
        let reg2 = if count == 4 {
            match unsafe { as_provider(arg(args, 3), "() argument 4") } {
                Ok(provider) => provider,
                Err(()) => {
                    unsafe {
                        drop_owned(out_reg);
                        drop_owned(reg1);
                    };
                    return std::ptr::null_mut();
                }
            }
        } else {
            std::ptr::null_mut()
        };
        // SAFETY: the receiver is a `ShaderBody`; the inputs are borrowed
        // objects the conversions handed over.
        let body = unsafe { &mut *self_.cast::<ShaderBody>() };
        // The output register must be a temporary; the two inputs may be
        // anything, each answered once per pass.
        let (out, _) = add_input(body, out_reg, true);
        let (in0, stored0) = add_input(body, reg1, false);
        let (in1, stored1) = add_input(body, reg2, false);
        body.instructions.push(Instruction {
            op: opcode,
            out,
            in0,
            in1,
        });
        // The inputs that were already registered keep the references the
        // conversions added for the rest of this call to release.
        if !stored0 {
            unsafe { drop_owned(reg1) };
        }
        if !stored1 {
            unsafe { drop_owned(reg2) };
        }
        unsafe { drop_owned(out_reg) };
        none_ref()
    }

    /// The `PY_BEGIN_ATTRIBUTES` table of `Vector4Shader`: none. The one
    /// method and the destruction entry are the type's whole surface.
    fn spec() -> binding::TypeSpec {
        binding::TypeSpec::new("Math.Vector4Shader")
            .base(v4provider::base_addr())
            .method("addOp", add_op)
            .dealloc(dealloc)
            .instance_size(INSTANCE_SIZE)
    }

    /// The shader's destruction entry: the held unique-input references go
    /// back, then the instruction and input lists, then the allocation.
    unsafe extern "C" fn dealloc(self_: *mut capi::PyObject) {
        // SAFETY: the receiver is a `ShaderBody`.
        let body = unsafe { &*self_.cast::<ShaderBody>() };
        for &input in &body.unique_inputs {
            unsafe { drop_owned(input) };
        }
        // SAFETY: dropping the body runs the two list destructors.
        unsafe {
            std::ptr::drop_in_place(self_.cast::<ShaderBody>());
            capi::PyObject_Free(self_.cast());
        }
    }

    /// `Vector4Shader::getRegister` (`:5026-5038`): the `r`-th temporary
    /// register provider, all sixty-three created on the first call, the
    /// `MAX_ARG( uint8, MAX_TEMP_REGISTERS - 1, END )` argument cap.
    unsafe extern "C" fn get_register(
        _module: *mut capi::PyObject,
        args: *mut capi::PyObject,
    ) -> *mut capi::PyObject {
        // SAFETY: the count read is on the borrowed arguments tuple.
        if unsafe { capi::PyTuple_Size(args) } != 1 {
            return type_error("getRegister() expects 1 argument of type uint8");
        }
        let Some(register) = (unsafe { as_uint8(arg(args, 0), "() argument 1") }) else {
            return std::ptr::null_mut();
        };
        if register as usize > MAX_TEMP_REGISTERS - 1 {
            return type_error("argument 1 is greater than maximum value");
        }
        let mut providers = TEMP_PROVIDERS
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if providers.is_empty() {
            // The reference's lazy init (`:5028-5034`): the first call
            // creates every temporary register at once.
            for index in 0..MAX_TEMP_REGISTERS {
                providers.push(v4register::make(index as u8) as usize);
            }
        }
        let provider = providers[register as usize] as *mut capi::PyObject;
        // SAFETY: the provider is a live object this module's statics hold;
        // the caller gets its own reference, the reference's `getData`.
        unsafe { capi::inc_ref(provider) };
        provider
    }

    /// The `Math.getRegister` module method descriptor; leaked for the
    /// module's lifetime, the reference's static method table.
    fn get_register_def() -> *mut capi::PyMethodDef {
        Box::leak(Box::new(capi::PyMethodDef {
            ml_name: c"getRegister".as_ptr() as *const c_char,
            ml_meth: Some(get_register),
            ml_flags: capi::METH_VARARGS,
            ml_doc: std::ptr::null(),
        }))
    }

    /// `TempPyVectorInit` for the shader: the type object, its `output`
    /// dispatch entry, the factory and the `Math.getRegister` module method
    /// into the `Math` module dictionary.
    pub(super) fn register() -> Result<PyRef, binding::BindError> {
        let type_object = binding::create_type(&spec())?;
        TYPE_ADDR.store(type_object.as_ptr() as usize, Ordering::SeqCst);
        v4provider::register_output(type_addr(), shader_output);

        // SAFETY: `register` runs from interpreter init; the interpreter is up.
        unsafe {
            let module = capi::PyImport_AddModule(c"Math".as_ptr());
            if !module.is_null() {
                let dict = capi::PyModule_GetDict(module);
                let factory = capi::PyCFunction_New(factory_def(), std::ptr::null_mut());
                if !factory.is_null() {
                    capi::PyDict_SetItemString(dict, c"Vector4Shader".as_ptr(), factory);
                    capi::dec_ref(factory);
                }
                let get_register = capi::PyCFunction_New(get_register_def(), std::ptr::null_mut());
                if !get_register.is_null() {
                    capi::PyDict_SetItemString(dict, c"getRegister".as_ptr(), get_register);
                    capi::dec_ref(get_register);
                }
            }
        }
        Ok(type_object)
    }
}

/// The `Math.Vector4Register` scratch register (`script_math.cpp:4816-4841`):
/// one temporary register of the shader file, answered by number. No
/// attributes and no factory, the reference's empty method and attribute
/// tables; `Math.getRegister` is the only way to one.
mod v4register {
    use super::*;

    /// The instance body beyond the `PyObject` header: the register number,
    /// the layout of the reference's `index_` (its `value_` is the shared
    /// file slot, which the port reads through [`v4shader::register_read`]).
    #[repr(C)]
    struct RegisterBody {
        header: capi::PyObject,
        index: u8,
    }

    const INSTANCE_SIZE: usize =
        std::mem::size_of::<RegisterBody>() - std::mem::size_of::<capi::PyObject>();

    /// The one `Math.Vector4Register` type object, by address.
    static TYPE_ADDR: AtomicUsize = AtomicUsize::new(0);

    fn type_addr() -> usize {
        TYPE_ADDR.load(Ordering::SeqCst)
    }

    /// `Vector4Register::Check` (`script.hpp:585`): is this object one of the
    /// temporary registers? `addInputVector` asks before it reads the index.
    pub(super) fn check(object: *mut capi::PyObject) -> bool {
        let address = type_addr();
        !object.is_null()
            && address != 0
            // SAFETY: the read is on the borrowed object.
            && unsafe { capi::Py_TYPE(object) as usize } == address
    }

    /// `Vector4Register::index()` (`:4828`): the register number.
    ///
    /// # Safety
    ///
    /// `check` answered true for this object.
    pub(super) unsafe fn index_of(object: *mut capi::PyObject) -> u8 {
        unsafe { std::ptr::addr_of!((*object.cast::<RegisterBody>()).index).read() }
    }

    /// `Vector4Register::output` (`:4827`): the register's current value.
    fn register_output(object: *mut capi::PyObject) -> [f32; 4] {
        // SAFETY: only a `Vector4Register` instance reaches this arm.
        v4shader::register_read(unsafe { index_of(object) })
    }

    /// A fresh `Math.Vector4Register` for one register number; the caller
    /// owns the reference. `getRegister`'s lazy init is the only caller.
    pub(super) fn make(index: u8) -> *mut capi::PyObject {
        let Ok(type_object) = binding::create_type(&spec()) else {
            return std::ptr::null_mut();
        };
        let Some(instance) = (unsafe { binding::instantiate(&type_object) }) else {
            return std::ptr::null_mut();
        };
        let pointer = instance.as_ptr();
        // SAFETY: the allocated body is exactly a `RegisterBody`.
        unsafe {
            std::ptr::addr_of_mut!((*pointer.cast::<RegisterBody>()).index).write(index);
        }
        std::mem::forget(instance);
        pointer
    }

    /// The `PY_BEGIN_METHODS`/`PY_BEGIN_ATTRIBUTES` tables of
    /// `Vector4Register`: both empty.
    fn spec() -> binding::TypeSpec {
        binding::TypeSpec::new("Math.Vector4Register")
            .base(v4provider::base_addr())
            .instance_size(INSTANCE_SIZE)
    }

    /// `TempPyVectorInit` for the register: the type object and its `output`
    /// dispatch entry. Not named in the `Math` module, the reference's
    /// `PY_TYPEOBJECT` shape with no `PY_FACTORY`.
    pub(super) fn register() -> Result<PyRef, binding::BindError> {
        let type_object = binding::create_type(&spec())?;
        TYPE_ADDR.store(type_object.as_ptr() as usize, Ordering::SeqCst);
        v4provider::register_output(type_addr(), register_output);
        Ok(type_object)
    }
}

/// The `Math.MatrixProduct` provider (`script_math.cpp:810-908`): the result
/// of `a x b`, the two held `MatrixProvider`s read in the reference's order,
/// an unassigned side the identity.
mod matrixproduct {
    use super::*;
    use foundation::math::Matrix;

    /// The instance body beyond the `PyObject` header: the two held
    /// `MatrixProvider`s, the layout of the reference's `MatrixProduct`.
    #[repr(C)]
    struct ProductBody {
        header: capi::PyObject,
        a: *mut capi::PyObject,
        b: *mut capi::PyObject,
    }

    const A: usize = std::mem::offset_of!(ProductBody, a);
    const B: usize = std::mem::offset_of!(ProductBody, b);

    const INSTANCE_SIZE: usize =
        std::mem::size_of::<ProductBody>() - std::mem::size_of::<capi::PyObject>();

    /// The one `Math.MatrixProduct` type object, by address.
    static TYPE_ADDR: AtomicUsize = AtomicUsize::new(0);

    fn type_addr() -> usize {
        TYPE_ADDR.load(Ordering::SeqCst)
    }

    /// `MatrixProduct::matrix` (`script_math.cpp:894-908`): `a` read first,
    /// `b` into a second matrix then `postMultiply`, an empty `a` the plain
    /// `b`, both empty the identity.
    fn product_matrix(object: *mut capi::PyObject) -> Matrix {
        let a = unsafe { matrix::slot_read::<A>(object) };
        let b = unsafe { matrix::slot_read::<B>(object) };
        if !a.is_null() {
            let mut m = matrix::provider_matrix(a).expect("a stored slot is a MatrixProvider");
            if !b.is_null() {
                let bm = matrix::provider_matrix(b).expect("a stored slot is a MatrixProvider");
                m.post_multiply(&bm);
            }
            m
        } else if !b.is_null() {
            matrix::provider_matrix(b).expect("a stored slot is a MatrixProvider")
        } else {
            Matrix::identity()
        }
    }

    /// A fresh `Math.MatrixProduct` over `a` and `b`; the caller owns the
    /// returned reference, and the fields take over `a` and `b`, the
    /// reference's empty `a_`/`b_` members.
    fn make(a: *mut capi::PyObject, b: *mut capi::PyObject) -> *mut capi::PyObject {
        let Ok(type_object) = binding::create_type(&spec()) else {
            return std::ptr::null_mut();
        };
        let Some(instance) = (unsafe { binding::instantiate(&type_object) }) else {
            return std::ptr::null_mut();
        };
        let pointer = instance.as_ptr();
        // SAFETY: the allocated body is exactly a `ProductBody`.
        unsafe {
            std::ptr::addr_of_mut!((*pointer.cast::<ProductBody>()).a).write(a);
            std::ptr::addr_of_mut!((*pointer.cast::<ProductBody>()).b).write(b);
        }
        std::mem::forget(instance);
        pointer
    }

    /// The `PY_AUTO_FACTORY_DECLARE( MatrixProduct, END )` factory: a fresh
    /// product with both slots empty, the reference's `END` argument list
    /// meaning no arguments.
    unsafe extern "C" fn factory(
        _module: *mut capi::PyObject,
        args: *mut capi::PyObject,
    ) -> *mut capi::PyObject {
        if !want_args(args, 0) {
            return type_error("Math.MatrixProduct() takes no arguments");
        }
        make(std::ptr::null_mut(), std::ptr::null_mut())
    }

    /// The factory descriptor for `PyCFunction_New`; leaked for the type's
    /// lifetime, the reference's static method table.
    fn factory_def() -> *mut capi::PyMethodDef {
        Box::leak(Box::new(capi::PyMethodDef {
            ml_name: c"MatrixProduct".as_ptr() as *const c_char,
            ml_meth: Some(factory),
            ml_flags: capi::METH_VARARGS,
            ml_doc: std::ptr::null(),
        }))
    }

    /// The `PY_BEGIN_ATTRIBUTES` table of `MatrixProduct`: the two held
    /// provider slots, the reference's
    /// `PY_RW_ATTRIBUTE_DECLARE( a_, a )` / `PY_RW_ATTRIBUTE_DECLARE( b_, b )`
    /// converter pairs.
    fn spec() -> binding::TypeSpec {
        binding::TypeSpec::new("Math.MatrixProduct")
            .base(matrix::provider_addr())
            .attribute_with(
                "a",
                matrix::slot_get::<A>,
                Some(matrix::slot_set::<A>),
                c"a".as_ptr() as *mut std::os::raw::c_void,
            )
            .attribute_with(
                "b",
                matrix::slot_get::<B>,
                Some(matrix::slot_set::<B>),
                c"b".as_ptr() as *mut std::os::raw::c_void,
            )
            .dealloc(dealloc)
            .instance_size(INSTANCE_SIZE)
    }

    provider_dealloc!(dealloc, ProductBody, a, b);

    /// `TempPyVectorInit` for the product: the type object, its `matrix()`
    /// dispatch entry, and the factory into the `Math` module dictionary.
    pub(super) fn register() -> Result<PyRef, binding::BindError> {
        let type_object = binding::create_type(&spec())?;
        TYPE_ADDR.store(type_object.as_ptr() as usize, Ordering::SeqCst);
        matrix::register_matrix_output(type_addr(), product_matrix);

        // SAFETY: `register` runs from interpreter init; the interpreter is up.
        unsafe {
            let module = capi::PyImport_AddModule(c"Math".as_ptr());
            if !module.is_null() {
                let dict = capi::PyModule_GetDict(module);
                let factory = capi::PyCFunction_New(factory_def(), std::ptr::null_mut());
                if !factory.is_null() {
                    capi::PyDict_SetItemString(dict, c"MatrixProduct".as_ptr(), factory);
                    capi::dec_ref(factory);
                }
            }
        }
        Ok(type_object)
    }
}

/// `TempPyVectorInit::init` (`script_math.cpp:3060-3065`): the `Math`
/// module with the three vector widths, the `MatrixProvider` base, the
/// `Matrix` and `Plane` types. A type that cannot be created reports and
/// leaves the rest registered, the shape of the reference's fire-and-forget
/// init-time job.
pub fn register() {
    let _ = vector2::register();
    let _ = vector3::register();
    let _ = vector4::register();
    let _ = matrix::register();
    let _ = plane::register();
    let _ = v4provider::register();
    let _ = v4product::register();
    let _ = v4animation::register();
    let _ = v4morph::register();
    let _ = v4translation::register();
    let _ = v4distance::register();
    let _ = v4lfo::register();
    let _ = v4adaptor::register();
    let _ = v4swizzle::register();
    let _ = v4combiner::register();
    let _ = v4shader::register();
    let _ = v4register::register();
    let _ = matrixproduct::register();
}
