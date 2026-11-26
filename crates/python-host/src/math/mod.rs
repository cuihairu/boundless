//! The `Math` module: script mathematics, the reference's `script_math.cpp`.
//!
//! The reference builds the module in init-time jobs at rung 0:
//! `TempPyVectorInit` (`lib/pyscript/script_math.cpp:3053-3067`) does
//! `PyImport_AddModule("Math")` and adds the three vector type objects, and
//! one `PyFactoryMethodLink` per remaining class (`py_factory_method_link.cpp:37-80`)
//! adds the class itself -- `Math.Matrix` is a real type whose `tp_new`
//! script calls reach, not a factory function -- rewriting each `tp_name`
//! to `Math.<name>` on the way (`:64-76`). Everything evaluates lazily: a
//! provider computes its value on every read (`Vector4Provider::pyGet_value`
//! `script_math.cpp:3190-3195`), and the operators of the vector classes
//! return fresh copies every time (`Script::getData`, `script.cpp:2311-2332`)
//! -- there is no temp-register reuse in vector arithmetic; the only
//! register machine in the reference is `Vector4Shader`'s shared static
//! register file (`script_math.cpp:4911-4912`).
//!
//! Error texts and coercion rules come from `Script::setData`
//! (`script.cpp:1752-1959`): floats accept `float`/`int` and say
//! `"<varName> must be set to a float"`, vectors accept their own class or
//! a tuple of the right width, enums accept only a string and list the
//! legal names in the error. Argument-count failures are `Script::argCountError`
//! (`script.cpp:2520-2568`): `"<name>() takes no arguments."` for the empty
//! case, `"<name>() expects N argument(s) of type(s) ..."` otherwise.
//!
//! The one deliberate addition a host needs: `ProviderStore` (`script_math.cpp:58-66`,
//! `:5329-5350`) is ticked by the host app once per frame
//! (`client/script_app.cpp:156`); [`tick`] is that step, and [`reset`] is
//! the fini-time teardown of everything this module holds.

// The vector and matrix classes are reused by the `ResMgr` faces
// (`readVector3`, `asMatrix` and friends build real `Math` objects), so
// the modules are crate-visible; `provider` stays module-private.
pub(crate) mod matrix;
mod provider;
pub(crate) mod vector;

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicPtr, Ordering};
use std::sync::{Mutex, OnceLock};

use crate::binding::TypeSpec;
use crate::capi;
use crate::pyref::PyRef;

/// The type objects this module created, by the Python-visible name: the
/// `Check` of every class is an exact type-object identity test
/// (`pyobject_plus.hpp:118-123`), which is this map keyed on the address.
///
/// Published as a copy-on-write snapshot behind an `AtomicPtr`: the reads
/// (`check_type`, `type_address`) sit on the hottest conversion path and
/// only ever take a shared snapshot, so the global mutex this table held
/// per lookup is gone. Writes happen only while the module is being
/// installed, from one thread, before any script runs; the superseded
/// snapshots are deliberately leaked (the same trade `binding.rs` makes
/// for its descriptor arrays) rather than reclaimed under an allocation a
/// reader may hold a reference into.
static TYPES: AtomicPtr<BTreeMap<&'static str, usize>> = AtomicPtr::new(std::ptr::null_mut());

/// The current snapshot of the table, or `None` before the first type is
/// recorded. A snapshot is immutable once published; the returned reference
/// is valid for the rest of the process.
fn type_snapshot() -> Option<&'static BTreeMap<&'static str, usize>> {
    let pointer = TYPES.load(Ordering::Acquire);
    if pointer.is_null() {
        None
    } else {
        // SAFETY: every pointer the registry ever stores was produced by
        // `Box::into_raw` in `remember_type` and is never freed.
        Some(unsafe { &*pointer })
    }
}

/// Record a created type under its Python name. Every call site passes a
/// name the compiler can prove outlives the program (the module's own
/// `&'static str` constants and `vector_name`'s), so the snapshot stores
/// the string's address without allocating an owned copy.
pub(super) fn remember_type(name: &'static str, object: &crate::pyref::PyRef) {
    let previous = TYPES.load(Ordering::Acquire);
    // SAFETY: as `type_snapshot` -- the previous snapshot, when present,
    // is a live leaked box.
    let base: BTreeMap<&'static str, usize> = match previous.is_null() {
        true => BTreeMap::new(),
        false => unsafe { &*previous }.clone(),
    };
    let mut next = base;
    next.insert(name, object.as_ptr() as usize);
    TYPES.store(Box::into_raw(Box::new(next)), Ordering::Release);
}

/// `Class::Check` (`pyobject_plus.hpp:118-123`): the object's type is
/// exactly the type this module created under `name`. The reference allows
/// subtypes, but no class here sets `Py_TPFLAGS_BASETYPE`
/// (`pyobject_plus.hpp:830-834`), so nothing can subclass them and exact
/// identity is the whole truth.
///
/// # Safety
///
/// `object` must be a borrowed object the interpreter handed out, or null.
pub(super) unsafe fn check_type(object: *mut capi::PyObject, name: &str) -> bool {
    if object.is_null() {
        return false;
    }
    match type_snapshot().and_then(|types| types.get(name).copied()) {
        Some(address) => (unsafe { capi::Py_TYPE(object) }) as usize == address,
        None => false,
    }
}

/// The address of a created type object, for hosts that need to build
/// instances of it.
pub(super) fn type_address(name: &str) -> Option<usize> {
    type_snapshot().and_then(|types| types.get(name).copied())
}

/// One instance teardown: drops the Rust body the instance carries, the
/// `~Class()` destructor step of the reference (which unregisters from the
/// provider store and releases held references before the memory goes).
type Teardown = unsafe fn(*mut capi::PyObject);

/// The teardown handlers, keyed on the type object's address.
static TEARDOWNS: OnceLock<Mutex<BTreeMap<usize, Teardown>>> = OnceLock::new();

fn teardowns() -> &'static Mutex<BTreeMap<usize, Teardown>> {
    TEARDOWNS.get_or_init(|| Mutex::new(BTreeMap::new()))
}

/// Register a body type for a created type object: the instance dealloc
/// runs `drop_in_place` on the body before the memory is released.
pub(super) fn register_teardown<T>(type_address: usize) {
    teardowns()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .insert(type_address, drop_body::<T>);
}

/// Register a custom teardown for a created type object: the provider types
/// use this for teardowns that also unregister the instance from the
/// provider store before the body drops.
pub(super) fn register_teardown_fn(address: usize, teardown: Teardown) {
    teardowns()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .insert(address, teardown);
}

/// The `drop_in_place` step for one body type, as a plain fn pointer.
unsafe fn drop_body<T>(object: *mut capi::PyObject) {
    // SAFETY: the dealloc runs exactly once per instance and the caller
    // guarantees the instance shape.
    unsafe { std::ptr::drop_in_place(body::<T>(object)) };
}

/// The `tp_dealloc` of the types that carry a Rust body: run the registered
/// teardown, release the memory block, and drop the heap type's own
/// reference -- the three steps the inherited heap-type dealloc does.
///
/// # Safety
///
/// A C `tp_dealloc` call: the interpreter holds the GIL and `object` has a
/// zero reference count.
unsafe extern "C" fn native_dealloc(object: *mut capi::PyObject) {
    let type_ = unsafe { capi::Py_TYPE(object) };
    let teardown = teardowns()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(&(type_ as usize))
        .copied();
    if let Some(teardown) = teardown {
        // SAFETY: the teardown is registered for exactly this type, and the
        // instance body is alive until the free below.
        unsafe { teardown(object) };
    }
    // The spec types are not garbage collected, so `PyType_GenericAlloc`'s
    // block is freed with `PyObject_Free` -- the `tp_free` `PyType_Ready`
    // assigns a non-GC spec type.
    // SAFETY: the block came from the same allocator.
    unsafe { capi::PyObject_Free(object.cast()) };
    // SAFETY: the type object is a heap type; the instance held one
    // reference of it.
    unsafe { capi::dec_ref(type_.cast()) };
}

/// Attach the body-carrying dealloc to a spec: the final builder step for
/// every type whose instances own Rust state.
pub(super) fn with_dealloc(spec: TypeSpec) -> TypeSpec {
    spec.slot(
        capi::Py_tp_dealloc,
        native_dealloc as *mut std::os::raw::c_void,
    )
}

/// A fresh boolean object, the `PyBool_FromLong` of the comparison answers.
pub(super) fn bool_object(value: bool) -> *mut capi::PyObject {
    if value {
        capi::py_true()
    } else {
        capi::py_false()
    }
}

/// One instance body: the bytes a `PyType_GenericAlloc` slot carries past the
/// `PyObject` header. The reference's instances are C++ objects allocated by
/// `new` (`pyobject_plus.cpp:51-62`); here the state lives in the tail the
/// allocation already reserved.
///
/// # Safety
///
/// `object` must be an instance of a type this module created with an
/// instance body of `T`, held while the interpreter is up.
pub(super) unsafe fn body<T>(object: *mut capi::PyObject) -> *mut T {
    // SAFETY: the allocation is `PyObject` header plus `size_of::<T>()`
    // bytes; the caller guarantees the instance shape.
    unsafe {
        (object
            .cast::<u8>()
            .add(std::mem::size_of::<capi::PyObject>()))
        .cast::<T>()
    }
}

/// `PyErr_SetString` with a Rust message.
pub(super) fn raise_type_error(message: &str) {
    // SAFETY: the message is a fresh unicode object and the exception is the
    // global `TypeError`.
    unsafe {
        let rendered = capi::unicode_from_bytes(message.as_bytes());
        capi::PyErr_SetObject(capi::PyExc_TypeError, rendered);
        capi::dec_ref(rendered);
    }
}

/// The `IndexError` the vector protocol raises.
pub(super) fn raise_index_error(message: &str) {
    // SAFETY: as above, with the global `IndexError`.
    unsafe {
        let rendered = capi::unicode_from_bytes(message.as_bytes());
        capi::PyErr_SetObject(capi::PyExc_IndexError, rendered);
        capi::dec_ref(rendered);
    }
}

/// The `ValueError` the mutable methods raise (`Matrix` inversion, enum
/// lookups).
pub(super) fn raise_value_error(message: &str) {
    // SAFETY: as above, with the global `ValueError`.
    unsafe {
        let rendered = capi::unicode_from_bytes(message.as_bytes());
        capi::PyErr_SetObject(capi::PyExc_ValueError, rendered);
        capi::dec_ref(rendered);
    }
}

/// The `argCountError` sentence (`script.cpp:2520-2568`): an empty
/// expectation is `"<name>() takes no arguments."`, anything else
/// `"<name>() expects N argument(s) of type(s) <types>"`.
pub(super) fn arg_count_sentence(name: &str, expected: &[&str]) -> String {
    let count = expected.len();
    if count == 0 {
        return format!("{name}() takes no arguments.");
    }
    let plural = if count == 1 { "" } else { "s" };
    format!(
        "{name}() expects {count} argument{plural} of type{plural} {}",
        expected.join(", ")
    )
}

/// The `argCountError` sentence for an optional tail: `"<name>() expects
/// between min and max arguments of types <types>"`, naming every accepted
/// type.
pub(super) fn arg_count_between_sentence(name: &str, min: usize, expected: &[&str]) -> String {
    let max = expected.len();
    format!(
        "{name}() expects between {min} and {max} arguments of types {}",
        expected.join(", ")
    )
}

/// `Script::argCountError` for the no-argument methods: sets the
/// `TypeError` and returns; the C handler answers the null result.
pub(super) fn takes_no_arguments(name: &str) {
    raise_type_error(&arg_count_sentence(name, &[]));
}

/// `Script::argCountError` for a fixed count: sets the `TypeError` and
/// returns; the C handler answers the null result.
pub(super) fn arg_count_error(name: &str, expected: &[&str]) {
    raise_type_error(&arg_count_sentence(name, expected));
}

/// `Script::argCountError` for an optional tail: sets the `TypeError` and
/// returns; the C handler answers the null result.
pub(super) fn arg_count_error_between(name: &str, min: usize, expected: &[&str]) {
    raise_type_error(&arg_count_between_sentence(name, min, expected));
}

/// `Script::setData(PyObject*, float&)` (`script.cpp:1805-1832`): a `float`
/// or an `int`; anything else is the pending `TypeError`.
///
/// # Safety
///
/// `object` must be a borrowed object the interpreter handed out, or null.
pub(super) unsafe fn coerce_float(object: *mut capi::PyObject, var_name: &str) -> Option<f64> {
    if unsafe { capi::py_float_check(object) } {
        // SAFETY: the check above guarantees a float object.
        return Some(unsafe { capi::PyFloat_AsDouble(object) });
    }
    if unsafe { capi::py_long_check(object) } {
        // The reference does not check the overflow error here
        // (`script.cpp:1820-1825`): a value past `long` reads as `-1`,
        // its `OverflowError` left pending — a stale indicator the
        // reference survives because it arms its trace callback inside
        // the profiling binaries alone, while this host installs
        // `_bw_profile` at init and no callback return can hide a
        // pending error from the armed monitoring shim (reproduced
        // against CPython 3.14). The value keeps the reference's `-1`
        // read; only the error this read itself raised is dropped, and
        // an error raised before the call — a sibling `coerce_float` in
        // the same parse, say `setPitchYaw('x', 1)` — is stashed across
        // it and handed back, the way the reference's parse stacks its
        // pending errors.
        // SAFETY: the check above guarantees an int object.
        let prior = unsafe { capi::PyErr_GetRaisedException() };
        let value = unsafe { capi::PyLong_AsLong(object) };
        unsafe { capi::clear_error() };
        if !prior.is_null() {
            // SAFETY: the stashed exception is owned and moves back to
            // the interpreter.
            unsafe { capi::PyErr_SetRaisedException(prior) };
        }
        return Some(value as f64);
    }
    raise_type_error(&format!("{var_name} must be set to a float"));
    None
}

/// `Script::setData(PyObject*, uint&)` (`script.cpp:1752-1793`): a
/// non-negative `int`, or a `float` truncated toward zero; anything else is
/// the pending `TypeError`.
///
/// # Safety
///
/// `object` must be a borrowed object the interpreter handed out, or null.
pub(super) unsafe fn coerce_uint(object: *mut capi::PyObject, var_name: &str) -> Option<u32> {
    if unsafe { capi::py_long_check(object) } {
        // SAFETY: the check above guarantees an int object.
        let value = unsafe { capi::PyLong_AsLong(object) };
        if value >= 0 {
            return Some(value as u32);
        }
    }
    if unsafe { capi::py_float_check(object) } {
        // SAFETY: the check above guarantees a float object.
        let value = unsafe { capi::PyFloat_AsDouble(object) };
        return Some(value as i32 as u32);
    }
    raise_type_error(&format!("{var_name} must be set to an uint"));
    None
}

/// `Script::setData(PyObject*, uint8&)`, the `INT_ACCESSOR( uint8, long )`
/// expansion of `script.hpp:200-224` on Linux: `setData` of the signed
/// 64-bit form first (`script.cpp:1673-1691` over `:1693-1734`), then the
/// widening comparison that only the accessor performs. A negative int is
/// converted, not refused: `uint8( -1 )` is `255`, which differs from the
/// signed `value`, so the reference answers `Integer is out of range`.
/// Anything that is neither an in-range `int` nor a `float` is the
/// `setData(int64&)` tail message, `must be set to a long`.
///
/// # Safety
///
/// `object` must be a borrowed object the interpreter handed out, or null.
pub(super) unsafe fn coerce_u8(object: *mut capi::PyObject, var_name: &str) -> Option<u8> {
    let value = if unsafe { capi::py_long_check(object) } {
        // SAFETY: the check above guarantees an int object.
        let converted = unsafe { capi::PyLong_AsLongLong(object) };
        if unsafe { capi::PyErr_Occurred() }.is_null() {
            Some(converted)
        } else {
            // The value does not fit an `i64`, so `PyErr_Occurred` is set:
            // the reference clears it and reports the ordinary failure
            // (`script.cpp:1700-1723`).
            unsafe { capi::clear_error() };
            None
        }
    } else if unsafe { capi::py_float_check(object) } {
        // SAFETY: the check above guarantees a float object.
        Some(unsafe { capi::PyFloat_AsDouble(object) } as i64)
    } else {
        raise_type_error(&format!("{var_name} must be set to a long"));
        return None;
    };
    let Some(value) = value else {
        raise_type_error(&format!("{var_name} must be set to a long"));
        return None;
    };
    let converted = value as u8;
    if (converted as i64) != value {
        raise_type_error("Integer is out of range");
        return None;
    }
    Some(converted)
}

/// `Script::getData(const float&)` (`script.cpp:2302-2305`): a fresh float
/// object the caller owns.
pub(super) fn float_object(value: f64) -> *mut capi::PyObject {
    // SAFETY: creating a float object is always safe while the interpreter
    // is up.
    unsafe { capi::PyFloat_FromDouble(value) }
}

/// Hand an owned handle to the interpreter as a borrowed C result: the
/// reference the handle owned becomes the result's own.
pub(super) fn own_raw(handle: PyRef) -> *mut capi::PyObject {
    let pointer = handle.as_ptr();
    std::mem::forget(handle);
    pointer
}

/// `PyVector<V>::pyStr` (`script_math.cpp:1362-1390`): `(x, y, z)` with each
/// component rendered the way a C++ `ostream` default does -- `%g` at six
/// significant digits, trailing zeros and point stripped.
pub(super) fn cpp_float(value: f64) -> String {
    if value == 0.0 {
        // Covers `-0.0` too, whose exponent form would carry a sign.
        return "0".to_owned();
    }
    // The non-finite values are the ostream words, not the `%.5e` render
    // (which carries no exponent for them): a vector divided by zero
    // reaches here through `pyStr` (`script_math.cpp:1362-1390`).
    if value.is_nan() {
        return "nan".to_owned();
    }
    if value.is_infinite() {
        return if value > 0.0 { "inf" } else { "-inf" }.to_owned();
    }
    // Style e at precision 5 decides the exponent after rounding, which is
    // the `%g` rule: six significant digits decide the style.
    let scientific = format!("{value:.5e}");
    let split = scientific.find('e').expect("the e of a scientific format");
    let exponent: i32 = scientific[split + 1..].parse().expect("an exponent");
    if (-4..6).contains(&exponent) {
        // Style f: enough digits so the total is six significant.
        let fixed = format!("{value:.*}", (5 - exponent).max(0) as usize);
        trim_number(&fixed).to_owned()
    } else {
        // Style e: the mantissa trimmed, a signed exponent at two digits --
        // `%g` always prints the sign (`1e-05`, `1.23457e+06`).
        let mantissa = trim_number(&scientific[..split]);
        let sign = if exponent < 0 { '-' } else { '+' };
        format!("{mantissa}e{sign}{:02}", exponent.abs())
    }
}

/// Strip trailing zeros and a trailing point from a fixed-format number,
/// the `%g` cleanup.
fn trim_number(rendered: &str) -> &str {
    if rendered.contains('.') {
        rendered.trim_end_matches('0').trim_end_matches('.')
    } else {
        rendered
    }
}

/// Build the whole `Math` module: the idempotent port of the reference's
/// rung-0 init jobs (`TempPyVectorInit`, one `PyFactoryMethodLink` per
/// class, the `getRegister` module link). Every name maps to one type
/// object for the process, so a repeat call re-attaches what exists.
pub fn install() {
    let module = unsafe { capi::PyImport_AddModule(c"Math".as_ptr()) };
    assert!(
        !module.is_null(),
        "Math: the module could not be created or found"
    );
    // SAFETY: the module object is live and this thread owns the GIL.
    unsafe {
        vector::install(module);
        matrix::install(module);
        provider::install(module);
    }
}

/// The fini-time teardown: drop everything this module holds while the
/// allocator is still alive, so the next interpreter starts clean. The
/// provider store releases its references first -- the teardown table must
/// still be intact for the deallocs that release fires -- then the
/// teardown and type tables clear, and `binding::reset` (the interpreter's
/// next step) releases the type objects themselves.
pub fn reset() {
    provider::reset();
    teardowns()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clear();
    // The snapshot registries retire by swinging the pointer back to null:
    // readers then see an empty table, the superseded snapshots stay
    // allocated (a fini-time reader may still hold one), and a subsequent
    // init starts from a fresh table.
    TYPES.store(std::ptr::null_mut(), Ordering::Release);
    vector::reset();
}

/// `ProviderStore::tick(dTime)` (`script_math.cpp:5329-5350`), the step the
/// host app runs once per frame (`client/script_app.cpp:156`): every
/// registered `Vector4` provider ticks, then every matrix provider, in
/// registration order.
pub fn tick(d_time: f32) {
    provider::tick(d_time);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The component format is the C++ `ostream` default: six significant
    /// digits, trailing zeros and the point stripped, scientific past six
    /// digits or below one ten-thousandth.
    #[test]
    fn components_render_like_the_cpp_ostream() {
        assert_eq!(cpp_float(0.0), "0");
        assert_eq!(cpp_float(1.0), "1");
        assert_eq!(cpp_float(2.5), "2.5");
        assert_eq!(cpp_float(-3.0), "-3");
        assert_eq!(cpp_float(0.333333_5f64), "0.333334");
        assert_eq!(cpp_float(1234567.0), "1.23457e+06");
        assert_eq!(cpp_float(0.00001), "1e-05");
        assert_eq!(cpp_float(0.0001), "0.0001");
        assert_eq!(cpp_float(123456.0), "123456");
    }

    /// The non-finite components are the ostream words: a vector divided
    /// by zero carries infinities through `pyStr`.
    #[test]
    fn the_non_finite_components_render_like_the_cpp_ostream() {
        assert_eq!(cpp_float(f64::INFINITY), "inf");
        assert_eq!(cpp_float(f64::NEG_INFINITY), "-inf");
        assert_eq!(cpp_float(f64::NAN), "nan");
    }

    /// The arg-count sentences are the reference's own, per shape.
    #[test]
    fn the_arg_count_errors_read_like_the_reference() {
        assert_eq!(
            arg_count_sentence("set", &["MatrixProvider"]),
            "set() expects 1 argument of type MatrixProvider"
        );
        assert_eq!(
            arg_count_sentence("lookAt", &["Vector3", "Vector3", "Vector3"]),
            "lookAt() expects 3 arguments of types Vector3, Vector3, Vector3"
        );
        assert_eq!(
            arg_count_between_sentence(
                "addOp",
                3,
                &[
                    "uint8",
                    "Vector4Provider",
                    "Vector4Provider",
                    "Vector4Provider"
                ],
            ),
            "addOp() expects between 3 and 4 arguments of types uint8, \
             Vector4Provider, Vector4Provider, Vector4Provider"
        );
    }

    /// An empty expectation is the no-arguments sentence.
    #[test]
    fn an_empty_expectation_takes_no_arguments() {
        assert_eq!(
            arg_count_sentence("setZero", &[]),
            "setZero() takes no arguments."
        );
    }

    /// The type table starts empty and `check_type` answers false on
    /// anything at all, before any interpreter exists.
    #[test]
    fn the_type_table_starts_empty() {
        assert!(type_address("Math.Vector3").is_none());
        // SAFETY: a null object is refused before the table is consulted.
        assert!(!unsafe { check_type(std::ptr::null_mut(), "Math.Vector3") });
    }
}
