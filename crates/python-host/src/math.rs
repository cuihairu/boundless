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

            fn type_addr() -> usize {
                TYPE_ADDR.load(Ordering::SeqCst)
            }

            /// A fresh instance carrying `data`; the caller owns the one
            /// reference. `Script::getData(V)`, the result arm of every
            /// arithmetic handler.
            fn make(data: [f32; N]) -> *mut capi::PyObject {
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

/// `TempPyVectorInit::init` (`script_math.cpp:3060-3065`): the `Math`
/// module with the three vector widths. A width that cannot be created
/// reports and leaves the rest registered, the shape of the reference's
/// fire-and-forget init-time job.
pub fn register() {
    let _ = vector2::register();
    let _ = vector3::register();
    let _ = vector4::register();
}
