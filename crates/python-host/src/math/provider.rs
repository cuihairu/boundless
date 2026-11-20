//! The `Vector4` providers and the remaining matrix providers, the
//! reference's `script_math.cpp:50-105` and `:3131-5230`.
//!
//! Every provider recomputes its value on each read
//! (`Vector4Provider::pyGet_value`, `script_math.cpp:3185-3191`), so a
//! chain of providers re-evaluates bottom-up in attribute order at every
//! read -- `a`'s output first, then `b`'s, then the combination. Ticking
//! follows [`super`]'s `ProviderStore` contract: the five `autoTick`
//! providers (Animation, Morph, Translation, Distance, LFO) register in
//! their constructor and tick in registration order, vectors before
//! matrices (`script_math.cpp:5329-5350`); every other class here is
//! static and never registers.
//!
//! Assignment coercion follows `Vector4Provider::coerce`
//! (`script_math.cpp:3262-3278`): a tuple of four floats or a `Vector4`
//! assigned to a provider attribute becomes a fresh `Vector4Basic`, a
//! provider passes through as a held reference, `None` clears the slot,
//! anything else is the pending `TypeError`. The two enum attributes
//! (LFO waveform, adaptor style, combiner fn) go through the
//! `PY_ENUM_CONVERTER_SET` contract (`script.hpp:824-860`): an exact
//! string match, `ValueError` listing the legal names for a wrong string,
//! `TypeError` for the same sentence when the value is not a string.
//!
//! Two deliberate deviations, both registered in the item's evidence
//! sentence: the reference's `keyframes` getter hands back a live
//! `PySTLSequence` view and its bad-item error embeds a mangled
//! `typeid` name (`stl_to_py.hpp:289-305`) -- the port answers a fresh
//! list and names the attribute instead; and `Vector4Shader`'s register
//! objects, static in the reference across interpreter restarts, are
//! rebuilt per interpreter with the register *values* kept.

use crate::binding::{self, TypeSpec};
use crate::capi;
use crate::pyref::PyRef;

use super::matrix::{MatrixValue, coerce_matrix_provider};
use super::vector::{parse_vector_like, vector_object};
use super::{
    arg_count_error, arg_count_error_between, body, check_type, coerce_float, coerce_uint,
    float_object, own_raw, raise_type_error, raise_value_error, register_teardown_fn,
    remember_type, takes_no_arguments, type_address, with_dealloc,
};

use std::ffi::c_int;
use std::sync::{Mutex, OnceLock};

// -----------------------------------------------------------------------------
// Section: the provider store
// -----------------------------------------------------------------------------

/// One registered provider's per-frame step: the virtual `tick(dTime)` the
/// store's loop dispatches (`script_math.cpp:5343-5349`).
type TickFn = unsafe fn(*mut capi::PyObject, f32);

/// The vector providers, in registration order. The reference keeps raw
/// `Vector4Provider*` entries (`ProviderStore::v4s_`,
/// `script_math.cpp:5287`); the port stores the object address with the
/// tick step that reads the body back, the vtable slot in table form.
static V4S: OnceLock<Mutex<Vec<(usize, TickFn)>>> = OnceLock::new();

/// The matrix providers, in registration order. Every matrix provider in
/// this module is static (`autoTick` false), so this list stays empty in
/// practice -- it exists for the store's shape (`:5288`).
static MS: OnceLock<Mutex<Vec<(usize, TickFn)>>> = OnceLock::new();

fn v4s() -> &'static Mutex<Vec<(usize, TickFn)>> {
    V4S.get_or_init(|| Mutex::new(Vec::new()))
}

fn ms() -> &'static Mutex<Vec<(usize, TickFn)>> {
    MS.get_or_init(|| Mutex::new(Vec::new()))
}

/// `ProviderStore::add(Vector4Provider*)` (`script_math.cpp:5295-5297`).
fn store_add_v4(object: *mut capi::PyObject, tick: TickFn) {
    v4s()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .push((object as usize, tick));
}

/// `ProviderStore::del` (`script_math.cpp:5307-5319`): find and erase the
/// first matching entry, silently ignoring an absent one.
fn store_del(object: *mut capi::PyObject) {
    let address = object as usize;
    let mut v4 = v4s()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(index) = v4.iter().position(|(slot, _)| *slot == address) {
        v4.remove(index);
        return;
    }
    let mut matrices = ms().lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(index) = matrices.iter().position(|(slot, _)| *slot == address) {
        matrices.remove(index);
    }
}

/// The teardown of one provider instance: leave the store first -- the
/// reference's destructor order (`~Vector4Provider`, `:3177-3182`) -- then
/// drop the body with its held references.
unsafe fn teardown_provider<T>(object: *mut capi::PyObject) {
    store_del(object);
    // SAFETY: the dealloc runs exactly once per instance and the caller
    // guarantees the instance shape.
    unsafe { std::ptr::drop_in_place(body::<T>(object)) };
}

/// `ProviderStore::tick(dTime)` (`script_math.cpp:5329-5350`): every
/// vector provider ticks, then every matrix provider, both in registration
/// order. The reference walks the live list while the tick runs; the port
/// snapshots the entries so a body step that drops a provider cannot read
/// freed memory, which no tick body here can make happen anyway.
pub(super) fn tick(d_time: f32) {
    let entries: Vec<(usize, TickFn)> = v4s()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone();
    for (address, step) in entries {
        // SAFETY: the address registered in the constructor and removed
        // only in the teardown, which the interpreter cannot run mid-tick.
        unsafe { step(address as *mut capi::PyObject, d_time) };
    }
    let entries: Vec<(usize, TickFn)> = ms()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone();
    for (address, step) in entries {
        // SAFETY: as above.
        unsafe { step(address as *mut capi::PyObject, d_time) };
    }
}

// -----------------------------------------------------------------------------
// Section: type identity and coercion
// -----------------------------------------------------------------------------

/// The dotted name of the static provider type scripts never name
/// directly: the coercion target the reference builds behind the scenes.
const BASIC: &str = "Math.Vector4Basic";

/// `Math.Vector4Product`.
const PRODUCT: &str = "Math.Vector4Product";

/// `Math.Vector4Animation`.
const ANIMATION: &str = "Math.Vector4Animation";

/// `Math.Vector4Morph`.
const MORPH: &str = "Math.Vector4Morph";

/// `Math.Vector4Translation`.
const TRANSLATION: &str = "Math.Vector4Translation";

/// `Math.Vector4Distance`.
const DISTANCE: &str = "Math.Vector4Distance";

/// `Math.Vector4LFO`.
const LFO: &str = "Math.Vector4LFO";

/// `Math.Vector4MatrixAdaptor`.
const ADAPTOR: &str = "Math.Vector4MatrixAdaptor";

/// `Math.Vector4Swizzle`.
const SWIZZLE: &str = "Math.Vector4Swizzle";

/// `Math.Vector4Combiner`.
const COMBINER: &str = "Math.Vector4Combiner";

/// `Math.Vector4Register`, the shader's register handle type.
const REGISTER: &str = "Math.Vector4Register";

/// `Math.Vector4Shader`.
const SHADER: &str = "Math.Vector4Shader";

/// `Math.MatrixProduct`.
const MATRIX_PRODUCT: &str = "Math.MatrixProduct";

/// `Math.MatrixInverse`.
const MATRIX_INVERSE: &str = "Math.MatrixInverse";

/// The `Vector4Provider::Check` set (`pyobject_plus.hpp:118-123`): every
/// concrete class under the vector-provider base. No class here is
/// subclassable, so exact type identity is the whole truth.
const V4_PROVIDERS: &[&str] = &[
    BASIC,
    PRODUCT,
    ANIMATION,
    MORPH,
    TRANSLATION,
    DISTANCE,
    LFO,
    SWIZZLE,
    COMBINER,
    REGISTER,
    SHADER,
];

/// The `MatrixProvider::Check` set: the matrix and the three provider
/// classes under it (`Vector4MatrixAdaptor` included,
/// `script_math.cpp:4318`).
const MATRIX_PROVIDERS: &[&str] = &["Math.Matrix", MATRIX_PRODUCT, MATRIX_INVERSE, ADAPTOR];

/// True when the object is an instance of one of the vector provider
/// types.
///
/// # Safety
///
/// `object` must be a borrowed object the interpreter handed out, or null.
pub(super) unsafe fn is_v4_provider(object: *mut capi::PyObject) -> bool {
    V4_PROVIDERS
        .iter()
        .any(|name| unsafe { check_type(object, name) })
}

/// True when the object is an instance of one of the matrix provider
/// types -- `MatrixProvider::Check`, the wall every matrix-provider
/// attribute and argument goes through.
///
/// # Safety
///
/// `object` must be a borrowed object the interpreter handed out, or null.
pub(super) unsafe fn is_matrix_provider(object: *mut capi::PyObject) -> bool {
    MATRIX_PROVIDERS
        .iter()
        .any(|name| unsafe { check_type(object, name) })
}

/// `Vector4Provider::coerce` as the attribute converter uses it
/// (`script_math.cpp:3262-3278` with the `PY_SCRIPT_CONVERTERS` store,
/// `:3283-3284`): `None` clears, a four-wide vector parse becomes a fresh
/// `Vector4Basic`, a provider is held as-is, anything else raises
/// `"<varName> must be set to a Vector4Provider or None"`. The `Option`
/// layer mirrors the nullable pointer: `None` means the argument was
/// `None`.
///
/// # Safety
///
/// `object` must be a borrowed object the interpreter handed out, or null.
unsafe fn coerce_v4_provider(object: *mut capi::PyObject, var_name: &str) -> Option<Option<PyRef>> {
    if std::ptr::eq(object, capi::py_none()) {
        return Some(None);
    }
    if let Some(values) = unsafe { parse_vector_like(object, 4, var_name) } {
        // The parse error is no longer interesting: the coercion falls
        // through to the provider test, the way `PyErr_Clear` does.
        unsafe { capi::clear_error() };
        return Some(Some(unsafe { make_basic(values) }));
    }
    unsafe { capi::clear_error() };
    if unsafe { is_v4_provider(object) } {
        // A held reference, the SmartPtr store step: a view of the argument,
        // then the clone whose fresh reference the slot takes.
        return Some(Some(PyRef::borrowed(object).clone()));
    }
    raise_type_error(&format!(
        "{var_name} must be set to a Vector4Provider or None"
    ));
    None
}

/// The matrix-provider attribute converter, the owned-reference form of
/// [`coerce_matrix_provider`]: the provider passes through as a held
/// reference.
///
/// # Safety
///
/// `object` must be a borrowed object the interpreter handed out, or null.
unsafe fn coerce_matrix_provider_owned(
    object: *mut capi::PyObject,
    var_name: &str,
) -> Option<Option<PyRef>> {
    match unsafe { coerce_matrix_provider(object, var_name) } {
        None => None,
        Some(None) => Some(None),
        // A view of the argument, then the clone: the view's `Drop` leaves
        // the caller's count alone, so the clone's reference goes to the
        // stored slot whole.
        Some(Some(raw)) => Some(Some(PyRef::borrowed(raw).clone())),
    }
}

/// Evaluate one provider slot: `None` for an empty slot, the provider's
/// output otherwise. The recursion that makes a provider chain re-evaluate
/// bottom-up.
///
/// # Safety
///
/// The interpreter must be up and the handle must name a provider of this
/// module, or be empty.
fn field_output(field: &Option<PyRef>) -> Option<[f32; 4]> {
    let handle = field.as_ref()?;
    Some(unsafe { provider_output(handle.as_ptr()) })
}

/// `Vector4Provider::output` dispatch: read the body of whichever provider
/// type the object is. The entry starts the output at zero -- the
/// deterministic stand-in for the reference's uninitialized `Vector4 val`
/// (`script_math.cpp:3186`), which a partial writer leaves as garbage.
///
/// # Safety
///
/// The interpreter must be up and `object` must be a provider of this
/// module.
unsafe fn provider_output(object: *mut capi::PyObject) -> [f32; 4] {
    let mut out = [0.0_f32; 4];
    if unsafe { check_type(object, BASIC) } {
        // SAFETY: the check above guarantees the instance shape.
        unsafe { (*body::<BasicBody>(object)).output(&mut out) };
    } else if unsafe { check_type(object, PRODUCT) } {
        // SAFETY: as above.
        unsafe { (*body::<ProductBody>(object)).output(&mut out) };
    } else if unsafe { check_type(object, ANIMATION) } {
        // SAFETY: as above.
        unsafe { (*body::<AnimationBody>(object)).output(&mut out) };
    } else if unsafe { check_type(object, MORPH) } {
        // SAFETY: as above.
        unsafe { (*body::<MorphBody>(object)).output(&mut out) };
    } else if unsafe { check_type(object, TRANSLATION) } {
        // SAFETY: as above.
        unsafe { (*body::<TranslationBody>(object)).output(&mut out) };
    } else if unsafe { check_type(object, DISTANCE) } {
        // SAFETY: as above.
        unsafe { (*body::<DistanceBody>(object)).output(&mut out) };
    } else if unsafe { check_type(object, LFO) } {
        // SAFETY: as above.
        unsafe { (*body::<LfoBody>(object)).output(&mut out) };
    } else if unsafe { check_type(object, SWIZZLE) } {
        // SAFETY: as above.
        unsafe { (*body::<SwizzleBody>(object)).output(&mut out) };
    } else if unsafe { check_type(object, COMBINER) } {
        // SAFETY: as above.
        unsafe { (*body::<CombinerBody>(object)).output(&mut out) };
    } else if unsafe { check_type(object, REGISTER) } {
        // SAFETY: as above.
        unsafe { (*body::<RegisterBody>(object)).output(&mut out) };
    } else if unsafe { check_type(object, SHADER) } {
        // SAFETY: as above.
        unsafe { (*body::<ShaderBody>(object)).output(&mut out) };
    }
    out
}

/// The `MatrixProvider::matrix` dispatch for the three provider classes
/// here: a fresh zero block filled by the object's body, the way every
/// reference caller builds a local `Matrix ma` first
/// (`script_math.cpp:3924-3930`). `None` for a non-provider object.
///
/// # Safety
///
/// The interpreter must be up and `object` must be a provider of this
/// module.
pub(super) unsafe fn matrix_of(object: *mut capi::PyObject) -> Option<MatrixValue> {
    let mut value = MatrixValue::zero();
    if unsafe { check_type(object, MATRIX_PRODUCT) } {
        // SAFETY: the check above guarantees the instance shape.
        unsafe { (*body::<MatrixProductBody>(object)).fill(&mut value) };
    } else if unsafe { check_type(object, MATRIX_INVERSE) } {
        // SAFETY: as above.
        unsafe { (*body::<MatrixInverseBody>(object)).fill(&mut value) };
    } else if unsafe { check_type(object, ADAPTOR) } {
        // SAFETY: as above.
        unsafe { (*body::<AdaptorBody>(object)).fill(&mut value) };
    } else {
        return None;
    }
    Some(value)
}

// -----------------------------------------------------------------------------
// Section: the componentwise Vector4 arithmetic
// -----------------------------------------------------------------------------

/// `XPVec4Lerp` (`script_math.cpp:3492-3496`): `a + t * (b - a)`,
/// componentwise.
fn lerp4(a: [f32; 4], b: [f32; 4], t: f32) -> [f32; 4] {
    [
        a[0] + t * (b[0] - a[0]),
        a[1] + t * (b[1] - a[1]),
        a[2] + t * (b[2] - a[2]),
        a[3] + t * (b[3] - a[3]),
    ]
}

/// `Vector4::parallelMultiply` (`vector4.ipp:225-240`).
fn parallel_multiply4(a: [f32; 4], b: [f32; 4]) -> [f32; 4] {
    [a[0] * b[0], a[1] * b[1], a[2] * b[2], a[3] * b[3]]
}

/// `Vector4::operator+` (`vector4.ipp`).
fn add4(a: [f32; 4], b: [f32; 4]) -> [f32; 4] {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2], a[3] + b[3]]
}

/// `Vector4::operator-`.
fn subtract4(a: [f32; 4], b: [f32; 4]) -> [f32; 4] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2], a[3] - b[3]]
}

/// `Vector4::operator*(Vector4, Vector4)`, the componentwise product.
fn multiply4(a: [f32; 4], b: [f32; 4]) -> [f32; 4] {
    parallel_multiply4(a, b)
}

/// `Vector4::operator/(Vector4, Vector4)`, the componentwise quotient
/// (`vector4.ipp`): a zero divisor cell produces the float infinity the
/// hardware would.
fn divide4(a: [f32; 4], b: [f32; 4]) -> [f32; 4] {
    [a[0] / b[0], a[1] / b[1], a[2] / b[2], a[3] / b[3]]
}

/// `Vector4::dotProduct` (`vector4.ipp`).
fn dot4(a: [f32; 4], b: [f32; 4]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2] + a[3] * b[3]
}

// -----------------------------------------------------------------------------
// Section: the enum tables
// -----------------------------------------------------------------------------

/// `Vector4LFO::Waveform` names, the `WAVE_` prefix stripped
/// (`script_math.cpp:4082-4089`).
const WAVEFORMS: &[&str] = &["SQUARE", "TRIANGLE", "SINE", "SAWTOOTH"];

/// `Vector4MatrixAdaptor::Style` names, the `STYLE_` prefix stripped
/// (`script_math.cpp:4333-4342`).
const STYLES: &[&str] = &[
    "XYZ_SCALE",
    "XY_SCALE",
    "X_ROTATE",
    "Y_ROTATE",
    "Z_ROTATE",
    "LOOKAT",
    "LOOKAT_SCALEZ",
];

/// `Vector4Combiner::Fn` names, the `FN_` prefix stripped
/// (`script_math.cpp:4660-4669`).
const FUNCTIONS: &[&str] = &["MULTIPLY", "DIVIDE", "ADD", "SUBTRACT", "DOT", "MIN", "MAX"];

/// The `PY_ENUM_CONVERTER_SET` sentence (`script.hpp:846-860`): the names
/// in map order, `", "` between and `", or "` before the last.
fn enum_sentence(var_name: &str, names: &[&str]) -> String {
    let mut sentence = format!("{var_name} must be set to one of ");
    for (index, name) in names.iter().enumerate() {
        if index > 0 {
            sentence += if index + 1 != names.len() {
                ", "
            } else {
                ", or "
            };
        }
        sentence += name;
    }
    sentence
}

/// `Script::setData(PyObject*, ENUMTYPE&, varName)` (`script.hpp:824-865`):
/// an exact string match names the slot; a wrong string is the
/// `ValueError`, a non-string the same sentence as a `TypeError`.
///
/// # Safety
///
/// `object` must be a borrowed object the interpreter handed out, or null.
unsafe fn parse_enum(
    names: &'static [&'static str],
    object: *mut capi::PyObject,
    var_name: &str,
) -> Option<usize> {
    let sentence = enum_sentence(var_name, names);
    if unsafe { capi::py_str_check(object) } {
        let text = unsafe { capi::to_string(object) };
        if let Some(index) = names.iter().position(|name| *name == text) {
            return Some(index);
        }
        raise_value_error(&sentence);
        return None;
    }
    raise_type_error(&sentence);
    None
}

/// `Script::getData(const ENUMTYPE)` (`script.hpp:867-882`): the name for
/// a live index, `None` past the table.
fn enum_object(names: &[&str], index: usize) -> *mut capi::PyObject {
    match names.get(index) {
        Some(name) => unsafe { capi::unicode_from_bytes(name.as_bytes()) },
        None => capi::py_none(),
    }
}

// -----------------------------------------------------------------------------
// Section: the shared accessors and construction helpers
// -----------------------------------------------------------------------------

/// The name an attribute setter reports: the attribute name, the
/// `PY_WRITABLE_ATTRIBUTE_SET` varName (`pyobject_plus.hpp:531-540`).
macro_rules! attr_name {
    ($field:ident) => {
        stringify!($field)
    };
}

/// One provider-slot attribute: a getter that clones the held reference
/// (or `None`), a setter that runs the coercion with the attribute name as
/// the var name. `PY_RW_ATTRIBUTE_DECLARE` for a `*ProviderPtr` field.
macro_rules! provider_accessors {
    ($get:ident, $set:ident, $body:ty, $field:ident, $coerce:expr) => {
        unsafe extern "C" fn $get(
            object: *mut capi::PyObject,
            _closure: *mut std::os::raw::c_void,
        ) -> *mut capi::PyObject {
            // SAFETY: the caller guarantees the instance shape.
            let field = unsafe { &*body::<$body>(object) }.$field.clone();
            match field {
                Some(handle) => own_raw(handle),
                None => capi::py_none(),
            }
        }
        unsafe extern "C" fn $set(
            object: *mut capi::PyObject,
            value: *mut capi::PyObject,
            _closure: *mut std::os::raw::c_void,
        ) -> c_int {
            let coerce: unsafe fn(*mut capi::PyObject, &str) -> Option<Option<PyRef>> = $coerce;
            let Some(coerced) = (unsafe { coerce(value, attr_name!($field)) }) else {
                return -1;
            };
            // SAFETY: the caller guarantees the instance shape.
            unsafe { (*body::<$body>(object)).$field = coerced };
            0
        }
    };
}

/// One plain-float attribute: a getter rendering the f32, a setter through
/// `Script::setData(PyObject*, float&)`. `PY_RW_ATTRIBUTE_DECLARE` for a
/// `float` field.
macro_rules! float_accessors {
    ($get:ident, $set:ident, $body:ty, $field:ident) => {
        unsafe extern "C" fn $get(
            object: *mut capi::PyObject,
            _closure: *mut std::os::raw::c_void,
        ) -> *mut capi::PyObject {
            // SAFETY: the caller guarantees the instance shape.
            float_object(unsafe { (*body::<$body>(object)).$field } as f64)
        }
        unsafe extern "C" fn $set(
            object: *mut capi::PyObject,
            value: *mut capi::PyObject,
            _closure: *mut std::os::raw::c_void,
        ) -> c_int {
            let Some(parsed) = (unsafe { coerce_float(value, attr_name!($field)) }) else {
                return -1;
            };
            // SAFETY: the caller guarantees the instance shape.
            unsafe { (*body::<$body>(object)).$field = parsed as f32 };
            0
        }
    };
}

/// One read-only float attribute: the getter half of
/// [`float_accessors!`], for the fields whose setter is a hand-written
/// body.
macro_rules! float_accessors_readonly {
    ($get:ident, $body:ty, $field:ident) => {
        unsafe extern "C" fn $get(
            object: *mut capi::PyObject,
            _closure: *mut std::os::raw::c_void,
        ) -> *mut capi::PyObject {
            // SAFETY: the caller guarantees the instance shape.
            float_object(unsafe { (*body::<$body>(object)).$field } as f64)
        }
    };
}

/// One enum attribute: a getter handing back the name, a setter through
/// the enum converter. `PY_RW_ATTRIBUTE_DECLARE` under a
/// `PY_DEFERRED_ATTRIBUTE_DECLARE` pair. The var name is the *attribute*
/// name, which `#NAME` in `PY_WRITABLE_ATTRIBUTE_SET` reports: the body
/// field is not always spelled like it (`fn` over the `function` field).
macro_rules! enum_accessors {
    ($get:ident, $set:ident, $body:ty, $field:ident, $names:expr, $var:literal) => {
        unsafe extern "C" fn $get(
            object: *mut capi::PyObject,
            _closure: *mut std::os::raw::c_void,
        ) -> *mut capi::PyObject {
            // SAFETY: the caller guarantees the instance shape.
            let index = unsafe { &*body::<$body>(object) }.$field;
            enum_object($names, index)
        }
        unsafe extern "C" fn $set(
            object: *mut capi::PyObject,
            value: *mut capi::PyObject,
            _closure: *mut std::os::raw::c_void,
        ) -> c_int {
            let Some(index) = (unsafe { parse_enum($names, value, $var) }) else {
                return -1;
            };
            // SAFETY: the caller guarantees the instance shape.
            unsafe { (*body::<$body>(object)).$field = index };
            0
        }
    };
}

/// The inherited `value` getter every vector provider carries from the
/// base class (`script_math.cpp:3159-3168`): `output` into a fresh
/// `Vector4`.
macro_rules! value_getter {
    ($get:ident) => {
        unsafe extern "C" fn $get(
            object: *mut capi::PyObject,
            _closure: *mut std::os::raw::c_void,
        ) -> *mut capi::PyObject {
            // SAFETY: the caller guarantees the instance shape and the
            // vector types are installed.
            let values = unsafe { provider_output(object) };
            own_raw(unsafe { vector_object(values, 4) })
        }
    };
}

/// The no-argument factory bodies (`PY_AUTO_FACTORY_DECLARE(.., END)`,
/// `script.hpp:1029-1046`): any argument at all is
/// `"X() takes no arguments."`, the `argCountError` empty branch
/// (`script.cpp:2546-2549`). False means the error was set and the
/// handler must answer null.
///
/// # Safety
///
/// `args` must be the argument tuple a C call handed over.
unsafe fn refuse_any_arguments(name: &str, args: *mut capi::PyObject) -> bool {
    if unsafe { capi::PyTuple_Size(args) } != 0 {
        takes_no_arguments(name);
        return false;
    }
    true
}

/// Allocate one instance of a created type and initialise its body: the
/// construction path every internal `new` takes
/// (`script_math.cpp:792-793` and friends).
///
/// # Safety
///
/// The interpreter must be up and `name` must be a type this module
/// created.
unsafe fn make_instance(name: &str, init: impl FnOnce(*mut capi::PyObject)) -> PyRef {
    let address = type_address(name).expect("the provider type is installed");
    let type_object = PyRef::borrowed(address as *mut capi::PyObject);
    // SAFETY: the type is one of this module's and the interpreter is up.
    let instance = unsafe { binding::instantiate(&type_object) }
        .expect("the provider instance allocation cannot fail");
    // SAFETY: the instance is the fresh allocation this call owns.
    init(instance.as_ptr());
    instance
}

/// Register an autoTick instance with the store: the base-constructor step
/// (`script_math.cpp:3172-3176`).
fn register_auto_tick(instance: &PyRef, tick: TickFn) {
    store_add_v4(instance.as_ptr(), tick);
}

/// The shared provider teardown builder: the type address gets a teardown
/// that leaves the store and drops the body.
fn provider_teardown<T>(address: usize) {
    register_teardown_fn(address, teardown_provider::<T>);
}

/// The tick steps, the vtable slots of the five autoTick bodies.
unsafe fn tick_animation(object: *mut capi::PyObject, d_time: f32) {
    // SAFETY: the registration guarantees the instance shape.
    unsafe { (*body::<AnimationBody>(object)).tick(d_time) };
}

/// # Safety
///
/// The registration guarantees the instance shape.
unsafe fn tick_morph(object: *mut capi::PyObject, d_time: f32) {
    // SAFETY: the registration guarantees the instance shape.
    unsafe { (*body::<MorphBody>(object)).tick(d_time) };
}

/// # Safety
///
/// The registration guarantees the instance shape.
unsafe fn tick_lfo(object: *mut capi::PyObject, d_time: f32) {
    // SAFETY: the registration guarantees the instance shape.
    unsafe { (*body::<LfoBody>(object)).tick(d_time) };
}

// -----------------------------------------------------------------------------
// Section: Vector4Basic
// -----------------------------------------------------------------------------

/// The `Vector4Basic` body (`script_math.cpp:3287-3291`): the constant.
struct BasicBody {
    values: [f32; 4],
}

impl BasicBody {
    fn output(&self, out: &mut [f32; 4]) {
        *out = self.values;
    }
}

// The `value` getter of the static provider (`script_math.cpp:3253-3261`
// through the base `pyGet_value`).
value_getter!(get_basic_value);

/// Build a `Vector4Basic` over a component set: the `coerce` step's
/// private construction.
///
/// # Safety
///
/// The interpreter must be up.
unsafe fn make_basic(values: [f32; 4]) -> PyRef {
    // SAFETY: the interpreter is up.
    unsafe {
        make_instance(BASIC, |instance| {
            // SAFETY: the instance is the fresh allocation this call owns.
            (*body::<BasicBody>(instance)).values = values;
        })
    }
}

// -----------------------------------------------------------------------------
// Section: Vector4Product
// -----------------------------------------------------------------------------

/// The `Vector4Product` body (`script_math.cpp:3296-3314`): the two
/// source slots.
struct ProductBody {
    a: Option<PyRef>,
    b: Option<PyRef>,
}

impl ProductBody {
    /// `Vector4Product::output` (`script_math.cpp:3415-3436`): the
    /// componentwise product of both sources, either source alone, or
    /// zero.
    fn output(&self, out: &mut [f32; 4]) {
        match (field_output(&self.a), field_output(&self.b)) {
            (Some(a), Some(b)) => *out = parallel_multiply4(a, b),
            (Some(a), None) => *out = a,
            (None, Some(b)) => *out = b,
            (None, None) => *out = [0.0; 4],
        }
    }
}

provider_accessors!(
    get_product_a,
    set_product_a,
    ProductBody,
    a,
    coerce_v4_provider
);
provider_accessors!(
    get_product_b,
    set_product_b,
    ProductBody,
    b,
    coerce_v4_provider
);
value_getter!(get_product_value);

/// # Safety
///
/// A C `tp_new` call: the interpreter holds the GIL.
unsafe extern "C" fn product_new(
    type_object: *mut capi::PyTypeObject,
    args: *mut capi::PyObject,
    _kwargs: *mut capi::PyObject,
) -> *mut capi::PyObject {
    if !unsafe { refuse_any_arguments("Vector4Product", args) } {
        return std::ptr::null_mut();
    }
    let handle = PyRef::borrowed(type_object.cast());
    // SAFETY: the type is one of this module's and the interpreter is up.
    let instance = unsafe { binding::instantiate(&handle) }
        .expect("the provider instance allocation cannot fail");
    // SAFETY: the instance is the fresh allocation this call owns. The
    // constructor nulls both sources (`script_math.cpp:3296-3314`: the
    // `a_( NULL ), b_( NULL )` initialiser list).
    unsafe {
        (*body::<ProductBody>(instance.as_ptr())) = ProductBody { a: None, b: None };
    }
    own_raw(instance)
}

// -----------------------------------------------------------------------------
// Section: Vector4Animation
// -----------------------------------------------------------------------------

/// The `Vector4Animation` body (`script_math.cpp:3466-3495`): the ordered
/// keyframes, the cycle clock and its length.
struct AnimationBody {
    keyframes: Vec<(f32, Option<PyRef>)>,
    time: f32,
    duration: f32,
}

impl AnimationBody {
    /// `Vector4Animation::tick` (`script_math.cpp:3645-3648`): the clock
    /// wraps at the duration, never below a millisecond.
    fn tick(&mut self, d_time: f32) {
        self.time = (self.time + d_time) % self.duration.max(0.001);
    }

    /// `Vector4Animation::output` (`script_math.cpp:3650-3691`): the
    /// linear walk -- the last keyframe at or before `time_`, the next
    /// one after, and the lerp between; before the first and after the
    /// last, the nearest keyframe alone.
    fn output(&self, out: &mut [f32; 4]) {
        if self.keyframes.is_empty() {
            *out = [0.0; 4];
            return;
        }
        // The walk: `oit` trails `nit`, which stops at the first keyframe
        // whose time is not before the clock.
        let mut previous = 0_usize;
        let mut next = 0_usize;
        while next < self.keyframes.len() && self.keyframes[next].0 < self.time {
            previous = next;
            next += 1;
        }
        if next == previous {
            // Before the first keyframe.
            if let Some(value) = field_output(&self.keyframes[next].1) {
                *out = value;
            }
        } else if next == self.keyframes.len() {
            // After the last keyframe.
            if let Some(value) = field_output(&self.keyframes[previous].1) {
                *out = value;
            }
        } else {
            let (before, after) = (self.keyframes[previous].0, self.keyframes[next].0);
            let Some(ov) = field_output(&self.keyframes[previous].1) else {
                return;
            };
            let Some(nv) = field_output(&self.keyframes[next].1) else {
                return;
            };
            let t = (self.time - before) / (after - before);
            *out = lerp4(ov, nv, t);
        }
    }
}

value_getter!(get_animation_value);

/// # Safety
///
/// A C getter call; the interpreter holds the GIL.
unsafe extern "C" fn get_animation_keyframes(
    object: *mut capi::PyObject,
    _closure: *mut std::os::raw::c_void,
) -> *mut capi::PyObject {
    // SAFETY: the caller guarantees the instance shape.
    let keyframes = &unsafe { &*body::<AnimationBody>(object) }.keyframes;
    // SAFETY: the interpreter is up.
    unsafe {
        let list = capi::PyList_New(keyframes.len() as isize);
        for (index, (time, provider)) in keyframes.iter().enumerate() {
            let time_object = float_object(*time as f64);
            let provider_object = match provider {
                Some(handle) => own_raw(handle.clone()),
                None => capi::py_none(),
            };
            let pair = capi::PyTuple_New(2);
            capi::PyTuple_SetItem(pair, 0, time_object);
            capi::PyTuple_SetItem(pair, 1, provider_object);
            capi::PyList_SetItem(list, index as isize, pair);
        }
        list
    }
}

/// # Safety
///
/// A C setter call; the interpreter holds the GIL.
unsafe extern "C" fn set_animation_keyframes(
    object: *mut capi::PyObject,
    value: *mut capi::PyObject,
    _closure: *mut std::os::raw::c_void,
) -> c_int {
    // `Script::setData(.., PySTLSequenceHolderBase&)` (`stl_to_py.cpp:422-431`):
    // the sequence wall first; a list reads as itself converted to a
    // tuple, `PySequence_Fast`'s job in the reference holder.
    let converted: Option<PyRef> = if unsafe { capi::py_tuple_check(value) } {
        None
    } else if unsafe { capi::py_list_check(value) } {
        // SAFETY: the list check above guarantees a list object; the
        // handle keeps the conversion alive for the reads below.
        match unsafe { PyRef::from_owned(capi::PyList_AsTuple(value)) } {
            Some(handle) => Some(handle),
            None => return -1,
        }
    } else {
        raise_type_error("keyframes must be set to a sequence");
        return -1;
    };
    let sequence = match &converted {
        Some(handle) => handle.as_ptr(),
        None => value,
    };
    let count = unsafe { capi::PyTuple_Size(sequence) };
    let mut parsed: Vec<(f32, Option<PyRef>)> = Vec::with_capacity(count.max(0) as usize);
    for index in 0..count {
        // SAFETY: the count above bounds the index.
        let item = unsafe { capi::PyTuple_GetItem(sequence, index) };
        // The keyframe pair converter (`script_math.cpp:3439-3458`): a
        // two-wide tuple of a float and a provider; anything else is the
        // pending TypeError. The reference names the item with the
        // `PySTLSequence` varName, which embeds a mangled `typeid` string
        // (`stl_to_py.hpp:289-305`); the port names the sequence itself.
        let var_name = "An item of a PySTLSequence of a keyframe";
        let good =
            unsafe { capi::py_tuple_check(item) } && unsafe { capi::PyTuple_Size(item) } == 2;
        if !good {
            raise_type_error(&format!(
                "{var_name} must be set to a tuple of a float and a Vector4Provider"
            ));
            return -1;
        }
        // SAFETY: the checks above bound the two indices.
        let (raw_time, raw_provider) = (unsafe { capi::PyTuple_GetItem(item, 0) }, unsafe {
            capi::PyTuple_GetItem(item, 1)
        });
        // The pair converter (`script_math.cpp:3441-3449`): the inner coercions
        // run first, then the *pair* sentence is formatted over whatever
        // they left pending, so a bad float or a bad provider both read as
        // the tuple wall rather than the inner wall.
        //
        // The two coercions run over the raw items, not the results: a
        // provider that is not a vector nor a provider clears the error the
        // float parse left behind, which would let the inner wall leak out
        // of the pair.
        let coerced = unsafe {
            let time = coerce_float(raw_time, var_name);
            capi::clear_error();
            let provider = coerce_v4_provider(raw_provider, var_name);
            capi::clear_error();
            match (time, provider) {
                (Some(time), Some(provider)) => Some((time, provider)),
                _ => {
                    raise_type_error(&format!(
                        "{var_name} must be set to a tuple of a float and a Vector4Provider"
                    ));
                    None
                }
            }
        };
        let Some((time, provider)) = coerced else {
            return -1;
        };
        // A null provider fails the pair, the `&& t.second` of the
        // converter (`script_math.cpp:3450`).
        if provider.is_none() {
            raise_type_error(&format!(
                "{var_name} must be set to a tuple of a float and a Vector4Provider"
            ));
            return -1;
        }
        parsed.push((time as f32, provider));
    }
    // The whole parse succeeded: commit, the holder's all-or-nothing
    // transaction (`stl_to_py.cpp:458-472`).
    // SAFETY: the caller guarantees the instance shape.
    unsafe { (*body::<AnimationBody>(object)).keyframes = parsed };
    0
}

float_accessors!(get_animation_time, set_animation_time, AnimationBody, time);
float_accessors!(
    get_animation_duration,
    set_animation_duration,
    AnimationBody,
    duration
);

/// # Safety
///
/// A C `tp_new` call: the interpreter holds the GIL.
unsafe extern "C" fn animation_new(
    type_object: *mut capi::PyTypeObject,
    args: *mut capi::PyObject,
    _kwargs: *mut capi::PyObject,
) -> *mut capi::PyObject {
    if !unsafe { refuse_any_arguments("Vector4Animation", args) } {
        return std::ptr::null_mut();
    }
    let handle = PyRef::borrowed(type_object.cast());
    // SAFETY: the type is one of this module's and the interpreter is up.
    let instance = unsafe { binding::instantiate(&handle) }
        .expect("the provider instance allocation cannot fail");
    // SAFETY: the instance is the fresh allocation this call owns.
    unsafe {
        (*body::<AnimationBody>(instance.as_ptr())) = AnimationBody {
            keyframes: Vec::new(),
            time: 0.0,
            duration: 1.0,
        };
    }
    register_auto_tick(&instance, tick_animation);
    own_raw(instance)
}

// -----------------------------------------------------------------------------
// Section: Vector4Morph
// -----------------------------------------------------------------------------

/// The `Vector4Morph` body (`script_math.cpp:3697-3723`): the target and
/// the value it morphs from, with the clock and its length.
struct MorphBody {
    target: [f32; 4],
    old: [f32; 4],
    time: f32,
    duration: f32,
}

impl MorphBody {
    /// `Vector4Morph::tick` (`script_math.cpp:3817-3820`).
    fn tick(&mut self, d_time: f32) {
        self.set_time(self.time + d_time);
    }

    /// `Vector4Morph::output` (`script_math.cpp:3825-3832`): the lerp of
    /// the clock across the duration.
    fn output(&self, out: &mut [f32; 4]) {
        let t = self.time / self.duration;
        for index in 0..4 {
            out[index] = (t * self.target[index]) + ((1.0 - t) * self.old[index]);
        }
    }

    /// `Vector4Morph::target(v)` (`script_math.cpp:3840-3846`): snapshot
    /// the current output as the old value, then reset the clock.
    fn set_target(&mut self, target: [f32; 4]) {
        let mut out = [0.0_f32; 4];
        self.output(&mut out);
        self.old = out;
        self.target = target;
        self.time = 0.0;
    }

    /// `Vector4Morph::duration(d)` (`script_math.cpp:3856-3867`): the
    /// progress ratio carries over, the floor is a tenth of a
    /// millisecond.
    fn set_duration(&mut self, duration: f32) {
        let ratio = self.time / self.duration;
        self.duration = duration.max(0.0001);
        self.set_time(ratio * self.duration);
    }

    /// `Vector4Morph::time(t)` (`script_math.cpp:3874-3877`): clamped
    /// between the floor and the duration.
    fn set_time(&mut self, time: f32) {
        self.time = time.clamp(0.0001, self.duration);
    }
}

value_getter!(get_morph_value);

/// # Safety
///
/// A C getter call; the interpreter holds the GIL.
unsafe extern "C" fn get_morph_target(
    object: *mut capi::PyObject,
    _closure: *mut std::os::raw::c_void,
) -> *mut capi::PyObject {
    // SAFETY: the caller guarantees the instance shape and the vector
    // types are installed.
    let target = unsafe { &*body::<MorphBody>(object) }.target;
    own_raw(unsafe { vector_object(target, 4) })
}

/// # Safety
///
/// A C setter call; the interpreter holds the GIL.
unsafe extern "C" fn set_morph_target(
    object: *mut capi::PyObject,
    value: *mut capi::PyObject,
    _closure: *mut std::os::raw::c_void,
) -> c_int {
    let Some(target) = (unsafe { parse_vector_like(value, 4, "target") }) else {
        return -1;
    };
    // SAFETY: the caller guarantees the instance shape.
    unsafe { (*body::<MorphBody>(object)).set_target(target) };
    0
}

// The `duration` attribute: the getter is the plain float read, the
// setter the carrying body (`script_math.cpp:3856-3867`).
float_accessors_readonly!(get_morph_duration, MorphBody, duration);

/// # Safety
///
/// A C setter call; the interpreter holds the GIL.
unsafe extern "C" fn set_morph_duration(
    object: *mut capi::PyObject,
    value: *mut capi::PyObject,
    _closure: *mut std::os::raw::c_void,
) -> c_int {
    let Some(duration) = (unsafe { coerce_float(value, "duration") }) else {
        return -1;
    };
    // SAFETY: the caller guarantees the instance shape.
    unsafe { (*body::<MorphBody>(object)).set_duration(duration as f32) };
    0
}

/// The `time` attribute carries the clamping setter
/// (`script_math.cpp:3874-3877`), not the plain float store.
///
/// # Safety
///
/// A C getter/setter pair; the interpreter holds the GIL.
unsafe extern "C" fn get_morph_time(
    object: *mut capi::PyObject,
    _closure: *mut std::os::raw::c_void,
) -> *mut capi::PyObject {
    // SAFETY: the caller guarantees the instance shape.
    float_object(unsafe { (*body::<MorphBody>(object)).time } as f64)
}

/// # Safety
///
/// A C setter call; the interpreter holds the GIL.
unsafe extern "C" fn set_morph_time(
    object: *mut capi::PyObject,
    value: *mut capi::PyObject,
    _closure: *mut std::os::raw::c_void,
) -> c_int {
    let Some(time) = (unsafe { coerce_float(value, "time") }) else {
        return -1;
    };
    // SAFETY: the caller guarantees the instance shape.
    unsafe { (*body::<MorphBody>(object)).set_time(time as f32) };
    0
}

/// # Safety
///
/// A C `tp_new` call: the interpreter holds the GIL.
unsafe extern "C" fn morph_new(
    type_object: *mut capi::PyTypeObject,
    args: *mut capi::PyObject,
    _kwargs: *mut capi::PyObject,
) -> *mut capi::PyObject {
    // `OPTARG(Vector4, Vector4(0,0,0,0), END)` (`script_math.cpp:3721`):
    // zero or one arguments.
    let count = unsafe { capi::PyTuple_Size(args) };
    if count > 1 {
        arg_count_error_between("Vector4Morph", 0, &["Vector4"]);
        return std::ptr::null_mut();
    }
    let mut original = [0.0_f32; 4];
    if count == 1 {
        // SAFETY: the count check above bounds the index.
        let item = unsafe { capi::PyTuple_GetItem(args, 0) };
        let Some(parsed) = (unsafe { parse_vector_like(item, 4, "() argument 1 optionally") })
        else {
            return std::ptr::null_mut();
        };
        original = parsed;
    }
    let handle = PyRef::borrowed(type_object.cast());
    // SAFETY: the type is one of this module's and the interpreter is up.
    let instance = unsafe { binding::instantiate(&handle) }
        .expect("the provider instance allocation cannot fail");
    // SAFETY: the instance is the fresh allocation this call owns.
    unsafe {
        (*body::<MorphBody>(instance.as_ptr())) = MorphBody {
            target: original,
            old: original,
            time: 0.0,
            duration: 1.0,
        };
    }
    register_auto_tick(&instance, tick_morph);
    own_raw(instance)
}

// -----------------------------------------------------------------------------
// Section: Vector4Translation and Vector4Distance
// -----------------------------------------------------------------------------

/// The `Vector4Translation` body (`script_math.cpp:3884-3896`): the source
/// matrix provider.
struct TranslationBody {
    source: Option<PyRef>,
}

impl TranslationBody {
    /// `Vector4Translation::output` (`script_math.cpp:4033-4039`): the
    /// origin of the source's matrix. A null source is the reference's
    /// crash -- `source_->matrix(ma)` on the NULL pointer the converter
    /// lets through -- which the port answers with a named abort.
    fn output(&self, out: &mut [f32; 4]) {
        let Some(handle) = &self.source else {
            panic!("Vector4Translation: the reference crashes on a null source here");
        };
        // SAFETY: the held reference guarantees a provider of this module.
        let matrix = unsafe { super::matrix::matrix_of(handle.as_ptr()) }
            .expect("a matrix provider answers its matrix");
        let origin = matrix.origin();
        *out = [origin[0], origin[1], origin[2], 1.0];
    }
}

provider_accessors!(
    get_translation_source,
    set_translation_source,
    TranslationBody,
    source,
    coerce_matrix_provider_owned
);
value_getter!(get_translation_value);

/// # Safety
///
/// A C `tp_new` call: the interpreter holds the GIL.
unsafe extern "C" fn translation_new(
    type_object: *mut capi::PyTypeObject,
    args: *mut capi::PyObject,
    _kwargs: *mut capi::PyObject,
) -> *mut capi::PyObject {
    // `ARG(MatrixProviderPtr, END)` (`script_math.cpp:3906`): exactly one.
    if unsafe { capi::PyTuple_Size(args) } != 1 {
        arg_count_error("Vector4Translation", &["MatrixProvider"]);
        return std::ptr::null_mut();
    }
    // SAFETY: the count check above bounds the index.
    let item = unsafe { capi::PyTuple_GetItem(args, 0) };
    let Some(source) = (unsafe { coerce_matrix_provider_owned(item, "() argument 1") }) else {
        return std::ptr::null_mut();
    };
    let handle = PyRef::borrowed(type_object.cast());
    // SAFETY: the type is one of this module's and the interpreter is up.
    let instance = unsafe { binding::instantiate(&handle) }
        .expect("the provider instance allocation cannot fail");
    // SAFETY: the instance is the fresh allocation this call owns.
    unsafe {
        (*body::<TranslationBody>(instance.as_ptr())) = TranslationBody { source };
    }
    register_auto_tick(&instance, tick_translation);
    own_raw(instance)
}

/// # Safety
///
/// The registration guarantees the instance shape.
unsafe fn tick_translation(object: *mut capi::PyObject, _d_time: f32) {
    // The reference's `Vector4Translation` inherits the empty base tick;
    // the registration is the whole effect.
    let _ = unsafe { body::<TranslationBody>(object) };
}

/// The `Vector4Distance` body (`script_math.cpp:4042-4054`): the two
/// matrix providers.
struct DistanceBody {
    a: Option<PyRef>,
    b: Option<PyRef>,
}

impl DistanceBody {
    /// `Vector4Distance::output` (`script_math.cpp:4132-4139`): the
    /// distance between the two origins, in every component. A null slot
    /// is the reference's crash, named here.
    fn output(&self, out: &mut [f32; 4]) {
        let (Some(first), Some(second)) = (&self.a, &self.b) else {
            panic!("Vector4Distance: the reference crashes on a null endpoint here");
        };
        // SAFETY: the held references guarantee providers of this module.
        let (ma, mb) = (
            unsafe { super::matrix::matrix_of(first.as_ptr()) }
                .expect("a matrix provider answers its matrix"),
            unsafe { super::matrix::matrix_of(second.as_ptr()) }
                .expect("a matrix provider answers its matrix"),
        );
        let (oa, ob) = (ma.origin(), mb.origin());
        let delta = [oa[0] - ob[0], oa[1] - ob[1], oa[2] - ob[2]];
        let length = (delta[0] * delta[0] + delta[1] * delta[1] + delta[2] * delta[2]).sqrt();
        *out = [length, length, length, length];
    }
}

provider_accessors!(
    get_distance_a,
    set_distance_a,
    DistanceBody,
    a,
    coerce_matrix_provider_owned
);
provider_accessors!(
    get_distance_b,
    set_distance_b,
    DistanceBody,
    b,
    coerce_matrix_provider_owned
);
value_getter!(get_distance_value);

/// # Safety
///
/// A C `tp_new` call: the interpreter holds the GIL.
unsafe extern "C" fn distance_new(
    type_object: *mut capi::PyTypeObject,
    args: *mut capi::PyObject,
    _kwargs: *mut capi::PyObject,
) -> *mut capi::PyObject {
    // `ARG(MatrixProviderPtr, ARG(MatrixProviderPtr, END))`:
    // exactly two.
    if unsafe { capi::PyTuple_Size(args) } != 2 {
        arg_count_error("Vector4Distance", &["MatrixProvider", "MatrixProvider"]);
        return std::ptr::null_mut();
    }
    // SAFETY: the count check above bounds both indices.
    let (first, second) = (unsafe { capi::PyTuple_GetItem(args, 0) }, unsafe {
        capi::PyTuple_GetItem(args, 1)
    });
    let Some(a) = (unsafe { coerce_matrix_provider_owned(first, "() argument 1") }) else {
        return std::ptr::null_mut();
    };
    let Some(b) = (unsafe { coerce_matrix_provider_owned(second, "() argument 2") }) else {
        return std::ptr::null_mut();
    };
    let handle = PyRef::borrowed(type_object.cast());
    // SAFETY: the type is one of this module's and the interpreter is up.
    let instance = unsafe { binding::instantiate(&handle) }
        .expect("the provider instance allocation cannot fail");
    // SAFETY: the instance is the fresh allocation this call owns.
    unsafe {
        (*body::<DistanceBody>(instance.as_ptr())) = DistanceBody { a, b };
    }
    register_auto_tick(&instance, tick_distance);
    own_raw(instance)
}

/// # Safety
///
/// The registration guarantees the instance shape.
unsafe fn tick_distance(object: *mut capi::PyObject, _d_time: f32) {
    let _ = unsafe { body::<DistanceBody>(object) };
}

// -----------------------------------------------------------------------------
// Section: Vector4LFO
// -----------------------------------------------------------------------------

/// The `Vector4LFO` body (`script_math.cpp:4073-4109`): the oscillator.
struct LfoBody {
    waveform: usize,
    period: f32,
    amplitude: f32,
    phase: f32,
    time: f32,
    parity: u8,
}

impl LfoBody {
    /// `Vector4LFO::tick` (`script_math.cpp:4230-4233`): the clock runs
    /// free, the parity flips every frame.
    fn tick(&mut self, d_time: f32) {
        self.time += d_time;
        self.parity = self.parity.wrapping_add(1);
    }

    /// `Vector4LFO::output` (`script_math.cpp:4238-4296`): the four
    /// waveforms, each folding the clock into the period first except the
    /// sine, all answering one scalar into all four components. A period
    /// inside `almostZero`'s default window (`stdmf.hpp:616-619`,
    /// epsilon 0.0004) silences the wave.
    fn output(&mut self, out: &mut [f32; 4]) {
        let x = match self.waveform {
            0 => {
                // SQUARE
                if almost_zero(self.period) {
                    0.0
                } else {
                    self.time %= self.period;
                    let t = self.time + self.phase;
                    let positive = if self.period > 0.0 {
                        t - self.period / 2.0 > 0.0
                    } else {
                        self.parity % 2 == 1
                    };
                    if positive { self.amplitude } else { 0.0 }
                }
            }
            1 => {
                // TRIANGLE
                if almost_zero(self.period) {
                    0.0
                } else {
                    self.time %= self.period;
                    let t = self.time + self.phase;
                    let f = 1.0 / self.period;
                    let positive = t - self.period / 2.0 > 0.0;
                    let mut x = (t * f) % 1.0;
                    if !positive {
                        x = 1.0 - x;
                    }
                    x * self.amplitude
                }
            }
            2 => {
                // SINE: the clock stays unwrapped; the phase-clocked angle
                // goes through a full turn.
                if almost_zero(self.period) {
                    0.0
                } else {
                    let t = (self.time + self.phase) % self.period;
                    let t = (t / self.period) * 2.0 * std::f32::consts::PI;
                    ((t.sin() * 0.5) + 0.5) * self.amplitude
                }
            }
            _ => {
                // SAWTOOTH
                if almost_zero(self.period) {
                    0.0
                } else {
                    self.time %= self.period;
                    let t = self.time + self.phase;
                    let f = 1.0 / self.period;
                    ((t * f) % 1.0) * self.amplitude
                }
            }
        };
        *out = [x, x, x, x];
    }
}

/// `almostZero(float)` with the default epsilon (`stdmf.hpp:616-619`).
fn almost_zero(value: f32) -> bool {
    -0.0004 < value && value < 0.0004
}

float_accessors!(get_lfo_period, set_lfo_period, LfoBody, period);
float_accessors!(get_lfo_amplitude, set_lfo_amplitude, LfoBody, amplitude);
float_accessors!(get_lfo_phase, set_lfo_phase, LfoBody, phase);
float_accessors!(get_lfo_time, set_lfo_time, LfoBody, time);
enum_accessors!(
    get_lfo_waveform,
    set_lfo_waveform,
    LfoBody,
    waveform,
    WAVEFORMS,
    "waveform"
);
value_getter!(get_lfo_value);

/// # Safety
///
/// A C `tp_new` call: the interpreter holds the GIL.
unsafe extern "C" fn lfo_new(
    type_object: *mut capi::PyTypeObject,
    args: *mut capi::PyObject,
    _kwargs: *mut capi::PyObject,
) -> *mut capi::PyObject {
    if !unsafe { refuse_any_arguments("Vector4LFO", args) } {
        return std::ptr::null_mut();
    }
    let handle = PyRef::borrowed(type_object.cast());
    // SAFETY: the type is one of this module's and the interpreter is up.
    let instance = unsafe { binding::instantiate(&handle) }
        .expect("the provider instance allocation cannot fail");
    // SAFETY: the instance is the fresh allocation this call owns. The
    // defaults are the constructor's (`script_math.cpp:4092-4100`): sine,
    // one second, full amplitude, no phase, no time, even parity.
    unsafe {
        (*body::<LfoBody>(instance.as_ptr())) = LfoBody {
            waveform: 2,
            period: 1.0,
            amplitude: 1.0,
            phase: 0.0,
            time: 0.0,
            parity: 0,
        };
    }
    register_auto_tick(&instance, tick_lfo);
    own_raw(instance)
}

// -----------------------------------------------------------------------------
// Section: Vector4MatrixAdaptor
// -----------------------------------------------------------------------------

/// The `Vector4MatrixAdaptor` body (`script_math.cpp:4300-4322`): the
/// source vector, the optional position, and the interpretation.
struct AdaptorBody {
    style: usize,
    source: Option<PyRef>,
    position: Option<PyRef>,
}

impl AdaptorBody {
    /// `Vector4MatrixAdaptor::matrix` (`script_math.cpp:4441-4500`): an
    /// empty source leaves the block untouched; every style composes its
    /// rotation or scale and lands the position in the translation.
    fn fill(&self, m: &mut MatrixValue) {
        let Some(source) = field_output(&self.source) else {
            return;
        };
        let tr = field_output(&self.position).unwrap_or([0.0; 4]);
        let pos = [tr[0], tr[1], tr[2]];
        match self.style {
            0 => {
                // XYZ_SCALE
                m.set_scale(source[0], source[1], source[2]);
                m.set_translation(pos);
            }
            1 => {
                // XY_SCALE
                m.set_scale(source[0], source[1], 1.0);
                m.set_translation(pos);
            }
            2 => {
                // X_ROTATE
                m.set_rotate_x(source[0]);
                m.set_translation(pos);
            }
            3 => {
                // Y_ROTATE
                m.set_rotate_y(source[1]);
                m.set_translation(pos);
            }
            4 => {
                // Z_ROTATE
                m.set_rotate_z(source[2]);
                m.set_translation(pos);
            }
            5 => {
                // LOOKAT: the up vector falls back to +Z when +Y looks
                // straight at the target.
                let at = [source[0], source[1], source[2]];
                let mut up = [0.0_f32, 1.0, 0.0];
                if up[0] * at[0] + up[1] * at[1] + up[2] * at[2] > 0.95 {
                    up = [0.0, 0.0, 1.0];
                }
                m.look_at(pos, at, up);
                m.invert();
            }
            _ => {
                // LOOKAT_SCALEZ: the look from the origin, then the
                // source's length folded into the z scale.
                let at = [source[0], source[1], source[2]];
                let mut up = [0.0_f32, 1.0, 0.0];
                if up[0] * at[0] + up[1] * at[1] + up[2] * at[2] > 0.95 {
                    up = [0.0, 0.0, 1.0];
                }
                m.look_at([0.0, 0.0, 0.0], at, up);
                m.invert();
                let scale =
                    (source[0] * source[0] + source[1] * source[1] + source[2] * source[2]).sqrt();
                let mut sc = MatrixValue::zero();
                sc.set_scale(1.0, 1.0, scale);
                m.pre_multiply(&sc);
                m.set_translation(pos);
            }
        }
    }
}

provider_accessors!(
    get_adaptor_source,
    set_adaptor_source,
    AdaptorBody,
    source,
    coerce_v4_provider
);
provider_accessors!(
    get_adaptor_position,
    set_adaptor_position,
    AdaptorBody,
    position,
    coerce_v4_provider
);
enum_accessors!(
    get_adaptor_style,
    set_adaptor_style,
    AdaptorBody,
    style,
    STYLES,
    "style"
);
value_getter!(get_adaptor_value);

/// # Safety
///
/// A C `tp_new` call: the interpreter holds the GIL.
unsafe extern "C" fn adaptor_new(
    type_object: *mut capi::PyTypeObject,
    args: *mut capi::PyObject,
    _kwargs: *mut capi::PyObject,
) -> *mut capi::PyObject {
    if !unsafe { refuse_any_arguments("Vector4MatrixAdaptor", args) } {
        return std::ptr::null_mut();
    }
    let handle = PyRef::borrowed(type_object.cast());
    // SAFETY: the type is one of this module's and the interpreter is up.
    let instance = unsafe { binding::instantiate(&handle) }
        .expect("the provider instance allocation cannot fail");
    // SAFETY: the instance is the fresh allocation this call owns. The
    // defaults are the constructor's (`script_math.cpp:4345-4351`):
    // XY_SCALE, no source, no position.
    unsafe {
        (*body::<AdaptorBody>(instance.as_ptr())) = AdaptorBody {
            style: 1,
            source: None,
            position: None,
        };
    }
    own_raw(instance)
}

// -----------------------------------------------------------------------------
// Section: Vector4Swizzle
// -----------------------------------------------------------------------------

/// The `Vector4Swizzle` body (`script_math.cpp:4526-4549`): the four
/// source slots.
struct SwizzleBody {
    x: Option<PyRef>,
    y: Option<PyRef>,
    z: Option<PyRef>,
    w: Option<PyRef>,
}

impl SwizzleBody {
    /// `Vector4Swizzle::output` (`script_math.cpp:4601-4620`): the x
    /// component of each source lands in its own slot; an empty source
    /// leaves that slot at the caller's zero (the reference reads
    /// uninitialized memory there).
    fn output(&self, out: &mut [f32; 4]) {
        if let Some(value) = field_output(&self.x) {
            out[0] = value[0];
        }
        if let Some(value) = field_output(&self.y) {
            out[1] = value[0];
        }
        if let Some(value) = field_output(&self.z) {
            out[2] = value[0];
        }
        if let Some(value) = field_output(&self.w) {
            out[3] = value[0];
        }
    }
}

provider_accessors!(
    get_swizzle_x,
    set_swizzle_x,
    SwizzleBody,
    x,
    coerce_v4_provider
);
provider_accessors!(
    get_swizzle_y,
    set_swizzle_y,
    SwizzleBody,
    y,
    coerce_v4_provider
);
provider_accessors!(
    get_swizzle_z,
    set_swizzle_z,
    SwizzleBody,
    z,
    coerce_v4_provider
);
provider_accessors!(
    get_swizzle_w,
    set_swizzle_w,
    SwizzleBody,
    w,
    coerce_v4_provider
);
value_getter!(get_swizzle_value);

/// # Safety
///
/// A C `tp_new` call: the interpreter holds the GIL.
unsafe extern "C" fn swizzle_new(
    type_object: *mut capi::PyTypeObject,
    args: *mut capi::PyObject,
    _kwargs: *mut capi::PyObject,
) -> *mut capi::PyObject {
    if !unsafe { refuse_any_arguments("Vector4Swizzle", args) } {
        return std::ptr::null_mut();
    }
    let handle = PyRef::borrowed(type_object.cast());
    // SAFETY: the type is one of this module's and the interpreter is up.
    let instance = unsafe { binding::instantiate(&handle) }
        .expect("the provider instance allocation cannot fail");
    // SAFETY: the instance is the fresh allocation this call owns.
    unsafe {
        (*body::<SwizzleBody>(instance.as_ptr())) = SwizzleBody {
            x: None,
            y: None,
            z: None,
            w: None,
        };
    }
    own_raw(instance)
}

// -----------------------------------------------------------------------------
// Section: Vector4Combiner
// -----------------------------------------------------------------------------

/// The `Vector4Combiner` body (`script_math.cpp:4627-4652`): the function
/// and the two inputs.
struct CombinerBody {
    function: usize,
    a: Option<PyRef>,
    b: Option<PyRef>,
}

impl CombinerBody {
    /// `Vector4Combiner::output` (`script_math.cpp:4763-4816`): both
    /// inputs present or the output stays at the caller's zero; the seven
    /// functions follow the componentwise operators, the dot spreading
    /// its scalar.
    fn output(&self, out: &mut [f32; 4]) {
        let (Some(a), Some(b)) = (field_output(&self.a), field_output(&self.b)) else {
            return;
        };
        *out = match self.function {
            0 => multiply4(a, b),
            1 => divide4(a, b),
            2 => add4(a, b),
            3 => subtract4(a, b),
            4 => {
                let s = dot4(a, b);
                [s, s, s, s]
            }
            5 => [
                a[0].min(b[0]),
                a[1].min(b[1]),
                a[2].min(b[2]),
                a[3].min(b[3]),
            ],
            _ => [
                a[0].max(b[0]),
                a[1].max(b[1]),
                a[2].max(b[2]),
                a[3].max(b[3]),
            ],
        };
    }
}

enum_accessors!(
    get_combiner_fn,
    set_combiner_fn,
    CombinerBody,
    function,
    FUNCTIONS,
    "fn"
);
provider_accessors!(
    get_combiner_a,
    set_combiner_a,
    CombinerBody,
    a,
    coerce_v4_provider
);
provider_accessors!(
    get_combiner_b,
    set_combiner_b,
    CombinerBody,
    b,
    coerce_v4_provider
);
value_getter!(get_combiner_value);

/// # Safety
///
/// A C `tp_new` call: the interpreter holds the GIL.
unsafe extern "C" fn combiner_new(
    type_object: *mut capi::PyTypeObject,
    args: *mut capi::PyObject,
    _kwargs: *mut capi::PyObject,
) -> *mut capi::PyObject {
    if !unsafe { refuse_any_arguments("Vector4Combiner", args) } {
        return std::ptr::null_mut();
    }
    let handle = PyRef::borrowed(type_object.cast());
    // SAFETY: the type is one of this module's and the interpreter is up.
    let instance = unsafe { binding::instantiate(&handle) }
        .expect("the provider instance allocation cannot fail");
    // SAFETY: the instance is the fresh allocation this call owns. The
    // default is ADD with empty inputs (`script_math.cpp:4645-4651`).
    unsafe {
        (*body::<CombinerBody>(instance.as_ptr())) = CombinerBody {
            function: 2,
            a: None,
            b: None,
        };
    }
    own_raw(instance)
}

// -----------------------------------------------------------------------------
// Section: Vector4Register and the register file
// -----------------------------------------------------------------------------

/// The static register file, `Vector4Shader::r_` (`script_math.cpp:4888`):
/// 128 cells, output and temporaries at 0-62, the unused cell at 63, user
/// inputs at 64 up.
fn registers() -> &'static Mutex<Box<[[f32; 4]; 128]>> {
    static REGISTERS: OnceLock<Mutex<Box<[[f32; 4]; 128]>>> = OnceLock::new();
    REGISTERS.get_or_init(|| Mutex::new(Box::new([[0.0; 4]; 128])))
}

/// A `PyRef` behind a static: the handles live only under the GIL, so
/// moving the raw pointer across threads is never exercised -- the
/// wrapper only makes the static's type system requirement true.
struct HandleSlot(PyRef);

// SAFETY: a `PyRef` is a raw object pointer; the static it feeds is only
// ever touched on the interpreter thread that owns the GIL.
unsafe impl Send for HandleSlot {}

/// The register handles, `Vector4Shader::pr_` (`script_math.cpp:4887`):
/// created on the first `getRegister` and kept for the process, each one
/// a live view of its cell.
fn register_handles() -> &'static Mutex<Vec<HandleSlot>> {
    static HANDLES: OnceLock<Mutex<Vec<HandleSlot>>> = OnceLock::new();
    HANDLES.get_or_init(|| Mutex::new(Vec::new()))
}

/// The `Vector4Register` body (`script_math.cpp:4846-4860`): the cell
/// index; the value lives in the shared file.
struct RegisterBody {
    index: u8,
}

impl RegisterBody {
    fn output(&self, out: &mut [f32; 4]) {
        // SAFETY: the interpreter is up and the file is a plain static.
        let file = registers()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *out = file[self.index as usize];
    }
}

value_getter!(get_register_value);

/// `Vector4Shader::getRegister` (`script_math.cpp:4911-4925`): the lazy
/// one-time construction of the 63 handles, then the shared handle for
/// the index.
///
/// # Safety
///
/// The interpreter must be up.
unsafe fn get_register(index: u8) -> PyRef {
    let mut handles = register_handles()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if handles.is_empty() {
        for cell in 0..63_u8 {
            // SAFETY: the interpreter is up.
            let handle = unsafe {
                make_instance(REGISTER, |instance| {
                    // SAFETY: the instance is the fresh allocation this
                    // call owns.
                    (*body::<RegisterBody>(instance)).index = cell;
                })
            };
            handles.push(HandleSlot(handle));
        }
    }
    handles[index as usize].0.clone()
}

/// `Math.getRegister` (`script_math.cpp:5026-5041`): the
/// `MAX_ARG(uint8, MAX_TEMP_REGISTERS - 1)` parse first
/// (`script.hpp:1153-1160`), then the shared handle.
///
/// # Safety
///
/// A C function call; the interpreter holds the GIL.
unsafe extern "C" fn method_get_register(
    _self_: *mut capi::PyObject,
    args: *mut capi::PyObject,
) -> *mut capi::PyObject {
    if unsafe { capi::PyTuple_Size(args) } != 1 {
        arg_count_error("getRegister", &["uint8"]);
        return std::ptr::null_mut();
    }
    // SAFETY: the count check above bounds the index.
    let item = unsafe { capi::PyTuple_GetItem(args, 0) };
    let Some(index) = (unsafe { coerce_uint(item, "() argument 1") }) else {
        return std::ptr::null_mut();
    };
    if index > 62 {
        raise_type_error("() argument 1 is greater than maximum value");
        return std::ptr::null_mut();
    }
    // SAFETY: the interpreter is up and the index is in range.
    own_raw(unsafe { get_register(index as u8) })
}

// -----------------------------------------------------------------------------
// Section: Vector4Shader
// -----------------------------------------------------------------------------

/// The unused-register code, `UNUSED_REGISTER` (`script_math.cpp:4883`).
const UNUSED_REGISTER: u8 = 63;

/// The first user-input register, `BASE_USER_REGISTER` (`:4884`).
const BASE_USER_REGISTER: u8 = 64;

/// The user-input cap, `NUM_USER_REGISTERS` (`:4882`).
const NUM_USER_REGISTERS: usize = 64;

/// One instruction, `Vector4Shader::Instruction` (`:4897-4903`): the
/// opcode, the temp register it writes, and the two inputs.
#[derive(Clone, Copy)]
struct ShaderIns {
    op: u8,
    out: u8,
    in0: u8,
    in1: u8,
}

/// The `Vector4Shader` body (`script_math.cpp:4906-4914`).
struct ShaderBody {
    instructions: Vec<ShaderIns>,
    unique_inputs: Vec<Option<PyRef>>,
}

impl ShaderBody {
    /// `Vector4Shader::addInputVector` (`script_math.cpp:4949-4999`): a
    /// null is the unused register, a `Vector4Register` its own index, a
    /// new provider the next user slot (deduplicated by identity, the
    /// SmartPtr equality of `std::find`).
    fn add_input_vector(&mut self, field: &Option<PyRef>, must_be_temp: bool) -> u8 {
        let Some(handle) = field else {
            return UNUSED_REGISTER;
        };
        let object = handle.as_ptr();
        // SAFETY: the held reference guarantees a provider of this module.
        if unsafe { check_type(object, REGISTER) } {
            // SAFETY: the check above guarantees the instance shape.
            return unsafe { (*body::<RegisterBody>(object)).index };
        }
        if must_be_temp {
            // The reference logs an `ERROR_MSG` here
            // (`script_math.cpp:4970-4972`); the port keeps the code.
            return UNUSED_REGISTER;
        }
        let address = object as usize;
        let known = self.unique_inputs.iter().position(|slot| {
            slot.as_ref()
                .is_some_and(|handle| handle.as_ptr() as usize == address)
        });
        if let Some(index) = known {
            return index as u8 + BASE_USER_REGISTER;
        }
        if self.unique_inputs.len() == NUM_USER_REGISTERS {
            // The reference logs and reuses the unused register
            // (`:4981-4986`).
            return UNUSED_REGISTER;
        }
        self.unique_inputs.push(Some(handle.clone()));
        (self.unique_inputs.len() as u8 - 1) + BASE_USER_REGISTER
    }

    /// `Vector4Shader::addOp` (`script_math.cpp:5107-5116`): the output
    /// must be a temp register, the two inputs anything.
    fn add_op(&mut self, op: u8, out: &Option<PyRef>, reg1: &Option<PyRef>, reg2: &Option<PyRef>) {
        let out = self.add_input_vector(out, true);
        let in0 = self.add_input_vector(reg1, false);
        let in1 = self.add_input_vector(reg2, false);
        self.instructions.push(ShaderIns { op, out, in0, in1 });
    }

    /// `Vector4Shader::output` (`script_math.cpp:5119-5224`): evaluate
    /// each unique input once into the user registers, then run every
    /// instruction over the shared file; the answer is always cell zero.
    fn output(&self, out: &mut [f32; 4]) {
        let mut file = registers()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        for (offset, slot) in self.unique_inputs.iter().enumerate() {
            if let Some(handle) = slot {
                // SAFETY: the held reference guarantees a provider of this
                // module.
                file[BASE_USER_REGISTER as usize + offset] =
                    unsafe { provider_output(handle.as_ptr()) };
            }
        }
        for instruction in &self.instructions {
            let i0 = file[instruction.in0 as usize];
            let i1 = file[instruction.in1 as usize];
            if let Some(result) = apply_shader_op(instruction.op, i0, i1) {
                file[instruction.out as usize] = result;
            }
        }
        *out = file[0];
    }
}

/// One opcode evaluation, the `switch` of `Vector4Shader::output`
/// (`script_math.cpp:5142-5220`). An opcode the switch has no case for
/// leaves the output cell untouched -- the reference's switch simply
/// assigns nothing -- so the result is `None` and the caller skips the
/// write.
fn apply_shader_op(op: u8, i0: [f32; 4], i1: [f32; 4]) -> Option<[f32; 4]> {
    Some(match op {
        0 => i0,
        1 => [1.0 / i0[0], 1.0 / i0[1], 1.0 / i0[2], 1.0 / i0[3]],
        2 => [i0[0] - 0.5, i0[1] - 0.5, i0[2] - 0.5, i0[3] - 0.5],
        3 => [1.0 - i0[0], 1.0 - i0[1], 1.0 - i0[2], 1.0 - i0[3]],
        4 => multiply4(i0, i1),
        5 => divide4(i0, i1),
        6 => add4(i0, i1),
        7 => subtract4(i0, i1),
        8 => {
            let s = dot4(i0, i1);
            [s, s, s, s]
        }
        9 => [
            i0[0].min(i1[0]),
            i0[1].min(i1[1]),
            i0[2].min(i1[2]),
            i0[3].min(i1[3]),
        ],
        10 => [
            i0[0].max(i1[0]),
            i0[1].max(i1[1]),
            i0[2].max(i1[2]),
            i0[3].max(i1[3]),
        ],
        11 => [
            if i0[0] >= i1[0] { 1.0 } else { 0.0 },
            if i0[0] >= i1[1] { 1.0 } else { 0.0 },
            if i0[0] >= i1[2] { 1.0 } else { 0.0 },
            if i0[0] >= i1[3] { 1.0 } else { 0.0 },
        ],
        12 => [
            if i0[0] < i1[0] { 1.0 } else { 0.0 },
            if i0[0] < i1[1] { 1.0 } else { 0.0 },
            if i0[0] < i1[2] { 1.0 } else { 0.0 },
            if i0[0] < i1[3] { 1.0 } else { 0.0 },
        ],
        _ => return None,
    })
}

value_getter!(get_shader_value);

/// The `addOp` parse (`script_math.cpp:5003-5005`): the opcode, the
/// output register, the first input, and the optional second input.
///
/// # Safety
///
/// A C method call; the interpreter holds the GIL.
unsafe extern "C" fn method_shader_add_op(
    object: *mut capi::PyObject,
    args: *mut capi::PyObject,
) -> *mut capi::PyObject {
    let count = unsafe { capi::PyTuple_Size(args) };
    if !(3..=4).contains(&count) {
        arg_count_error_between(
            "addOp",
            3,
            &[
                "uint8",
                "Vector4Provider",
                "Vector4Provider",
                "Vector4Provider",
            ],
        );
        return std::ptr::null_mut();
    }
    // SAFETY: the count check above bounds every index used.
    let opcode = unsafe { capi::PyTuple_GetItem(args, 0) };
    let out_reg = unsafe { capi::PyTuple_GetItem(args, 1) };
    let reg1 = unsafe { capi::PyTuple_GetItem(args, 2) };
    let Some(opcode) = (unsafe { coerce_uint(opcode, "() argument 1") }) else {
        return std::ptr::null_mut();
    };
    let Some(out) = (unsafe { coerce_v4_provider(out_reg, "() argument 2") }) else {
        return std::ptr::null_mut();
    };
    let Some(reg1) = (unsafe { coerce_v4_provider(reg1, "() argument 3") }) else {
        return std::ptr::null_mut();
    };
    let reg2 = if count == 4 {
        // SAFETY: the count check above bounds the index.
        let item = unsafe { capi::PyTuple_GetItem(args, 3) };
        let Some(parsed) = (unsafe { coerce_v4_provider(item, "() argument 4 optionally") }) else {
            return std::ptr::null_mut();
        };
        parsed
    } else {
        None
    };
    // SAFETY: the caller guarantees the instance shape.
    unsafe {
        (*body::<ShaderBody>(object)).add_op(opcode as u8, &out, &reg1, &reg2);
    }
    capi::py_none()
}

/// # Safety
///
/// A C `tp_new` call: the interpreter holds the GIL.
unsafe extern "C" fn shader_new(
    type_object: *mut capi::PyTypeObject,
    args: *mut capi::PyObject,
    _kwargs: *mut capi::PyObject,
) -> *mut capi::PyObject {
    if !unsafe { refuse_any_arguments("Vector4Shader", args) } {
        return std::ptr::null_mut();
    }
    let handle = PyRef::borrowed(type_object.cast());
    // SAFETY: the type is one of this module's and the interpreter is up.
    let instance = unsafe { binding::instantiate(&handle) }
        .expect("the provider instance allocation cannot fail");
    // SAFETY: the instance is the fresh allocation this call owns.
    unsafe {
        (*body::<ShaderBody>(instance.as_ptr())) = ShaderBody {
            instructions: Vec::new(),
            unique_inputs: Vec::new(),
        };
    }
    own_raw(instance)
}

// -----------------------------------------------------------------------------
// Section: MatrixProduct and MatrixInverse
// -----------------------------------------------------------------------------

/// The `MatrixProduct` body (`script_math.cpp:822-830`): the two slots of
/// the `a x b` expression.
struct MatrixProductBody {
    a: Option<PyRef>,
    b: Option<PyRef>,
}

impl MatrixProductBody {
    /// `MatrixProduct::matrix` (`script_math.cpp:900-920`): both slots,
    /// either slot, or the identity.
    fn fill(&self, m: &mut MatrixValue) {
        match (&self.a, &self.b) {
            (Some(a), Some(b)) => {
                // SAFETY: the held references guarantee providers.
                let (ma, mb) = (
                    unsafe { super::matrix::matrix_of(a.as_ptr()) }
                        .expect("a matrix provider answers its matrix"),
                    unsafe { super::matrix::matrix_of(b.as_ptr()) }
                        .expect("a matrix provider answers its matrix"),
                );
                let mut tmp = MatrixValue::zero();
                tmp.multiply(&ma, &mb);
                *m = tmp;
            }
            (Some(a), None) => {
                // SAFETY: as above.
                *m = unsafe { super::matrix::matrix_of(a.as_ptr()) }
                    .expect("a matrix provider answers its matrix");
            }
            (None, Some(b)) => {
                // SAFETY: as above.
                *m = unsafe { super::matrix::matrix_of(b.as_ptr()) }
                    .expect("a matrix provider answers its matrix");
            }
            (None, None) => *m = MatrixValue::identity(),
        }
    }
}

provider_accessors!(
    get_matrix_product_a,
    set_matrix_product_a,
    MatrixProductBody,
    a,
    coerce_matrix_provider_owned
);
provider_accessors!(
    get_matrix_product_b,
    set_matrix_product_b,
    MatrixProductBody,
    b,
    coerce_matrix_provider_owned
);

/// # Safety
///
/// A C `tp_new` call: the interpreter holds the GIL.
unsafe extern "C" fn matrix_product_new(
    type_object: *mut capi::PyTypeObject,
    args: *mut capi::PyObject,
    _kwargs: *mut capi::PyObject,
) -> *mut capi::PyObject {
    if !unsafe { refuse_any_arguments("MatrixProduct", args) } {
        return std::ptr::null_mut();
    }
    let handle = PyRef::borrowed(type_object.cast());
    // SAFETY: the type is one of this module's and the interpreter is up.
    let instance = unsafe { binding::instantiate(&handle) }
        .expect("the provider instance allocation cannot fail");
    // SAFETY: the instance is the fresh allocation this call owns.
    unsafe {
        (*body::<MatrixProductBody>(instance.as_ptr())) = MatrixProductBody { a: None, b: None };
    }
    own_raw(instance)
}

/// The `MatrixInverse` body (`script_math.cpp:940-952`): the source slot.
struct MatrixInverseBody {
    source: Option<PyRef>,
}

impl MatrixInverseBody {
    /// `MatrixInverse::matrix` (`script_math.cpp:1014-1026`): the source
    /// inverted, or the identity.
    fn fill(&self, m: &mut MatrixValue) {
        let Some(handle) = &self.source else {
            *m = MatrixValue::identity();
            return;
        };
        // SAFETY: the held reference guarantees a provider.
        let matrix = unsafe { super::matrix::matrix_of(handle.as_ptr()) }
            .expect("a matrix provider answers its matrix");
        m.invert_from(&matrix);
    }
}

provider_accessors!(
    get_matrix_inverse_source,
    set_matrix_inverse_source,
    MatrixInverseBody,
    source,
    coerce_matrix_provider_owned
);

/// # Safety
///
/// A C `tp_new` call: the interpreter holds the GIL.
unsafe extern "C" fn matrix_inverse_new(
    type_object: *mut capi::PyTypeObject,
    args: *mut capi::PyObject,
    _kwargs: *mut capi::PyObject,
) -> *mut capi::PyObject {
    // `ARG(MatrixProviderPtr, END)` (`script_math.cpp:950`): exactly one.
    if unsafe { capi::PyTuple_Size(args) } != 1 {
        arg_count_error("MatrixInverse", &["MatrixProvider"]);
        return std::ptr::null_mut();
    }
    // SAFETY: the count check above bounds the index.
    let item = unsafe { capi::PyTuple_GetItem(args, 0) };
    let Some(source) = (unsafe { coerce_matrix_provider_owned(item, "() argument 1") }) else {
        return std::ptr::null_mut();
    };
    let handle = PyRef::borrowed(type_object.cast());
    // SAFETY: the type is one of this module's and the interpreter is up.
    let instance = unsafe { binding::instantiate(&handle) }
        .expect("the provider instance allocation cannot fail");
    // SAFETY: the instance is the fresh allocation this call owns.
    unsafe {
        (*body::<MatrixInverseBody>(instance.as_ptr())) = MatrixInverseBody { source };
    }
    own_raw(instance)
}

// -----------------------------------------------------------------------------
// Section: installation
// -----------------------------------------------------------------------------

/// The dotted name of a class, as the module dictionary spells it.
fn bare_name(name: &str) -> std::ffi::CString {
    std::ffi::CString::new(name.strip_prefix("Math.").expect("the prefix"))
        .expect("no embedded NUL")
}

/// Attach one created type to the `Math` module dictionary -- the
/// `PyFactoryMethodLink::init` step that also rewrites the type's
/// `tp_name` to the qualified form (`py_factory_method_link.cpp:36-80`).
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

/// Build the whole provider family: the type objects of the classes the
/// factory links add (`script_math.cpp:894`, `:1008`, `:3395`, `:3630`,
/// `:3812`, `:3953`, `:4048`, `:4228`, `:4430`, `:4599`, `:4756`,
/// `:5014`) plus the two unattached ones the coercion and register
/// machinery build (`Vector4Basic`, `Vector4Register`).
///
/// `Math.MatrixAnimation` stays out: its class body is inside the
/// Windows-only block (`script_math.cpp:1040-1248`), so the Linux build
/// never had it.
///
/// # Safety
///
/// The interpreter must be up on the calling thread.
pub(super) unsafe fn install(module: *mut capi::PyObject) {
    // The attached classes, in factory-link declaration order.
    for (name, spec) in [
        (PRODUCT, product_spec()),
        (ANIMATION, animation_spec()),
        (MORPH, morph_spec()),
        (TRANSLATION, translation_spec()),
        (DISTANCE, distance_spec()),
        (LFO, lfo_spec()),
        (ADAPTOR, adaptor_spec()),
        (SWIZZLE, swizzle_spec()),
        (COMBINER, combiner_spec()),
        (SHADER, shader_spec()),
        (MATRIX_PRODUCT, matrix_product_spec()),
        (MATRIX_INVERSE, matrix_inverse_spec()),
    ] {
        // The bodies own references: every provider type gets the
        // body-carrying dealloc, and a teardown that leaves the store
        // first (a no-op find for the static ones, the destructor order
        // of the reference either way).
        let spec = with_dealloc(spec);
        // SAFETY: the interpreter is up.
        let object = match binding::create_type(&spec) {
            Ok(object) => object,
            Err(_) => panic!("Math: the {name} type could not be created"),
        };
        remember_type(name, &object);
        // SAFETY: the module is live.
        unsafe { attach(module, bare_name(name), &object) };
        let address = object.as_ptr() as usize;
        match name {
            PRODUCT => provider_teardown::<ProductBody>(address),
            ANIMATION => provider_teardown::<AnimationBody>(address),
            MORPH => provider_teardown::<MorphBody>(address),
            TRANSLATION => provider_teardown::<TranslationBody>(address),
            DISTANCE => provider_teardown::<DistanceBody>(address),
            LFO => provider_teardown::<LfoBody>(address),
            ADAPTOR => provider_teardown::<AdaptorBody>(address),
            SWIZZLE => provider_teardown::<SwizzleBody>(address),
            COMBINER => provider_teardown::<CombinerBody>(address),
            SHADER => provider_teardown::<ShaderBody>(address),
            MATRIX_PRODUCT => provider_teardown::<MatrixProductBody>(address),
            MATRIX_INVERSE => provider_teardown::<MatrixInverseBody>(address),
            _ => unreachable!("the spec table names a provider type"),
        }
        drop(object);
    }
    // The two unattached types: the coercion target and the register
    // handles. No factory link, so no module dictionary entry.
    for (name, spec, body_drop) in [
        (BASIC, basic_spec(), false),
        (REGISTER, register_spec(), true),
    ] {
        let spec = with_dealloc(spec);
        // SAFETY: the interpreter is up.
        let object = match binding::create_type(&spec) {
            Ok(object) => object,
            Err(_) => panic!("Math: the {name} type could not be created"),
        };
        remember_type(name, &object);
        let address = object.as_ptr() as usize;
        if body_drop {
            provider_teardown::<RegisterBody>(address);
        } else {
            super::register_teardown::<BasicBody>(address);
        }
        drop(object);
    }
    // `Math.getRegister`, the one module-level function of the family
    // (`script_math.cpp:5051`).
    let definition = Box::leak(Box::new(capi::PyMethodDef {
        ml_name: c"getRegister".as_ptr(),
        ml_meth: Some(method_get_register),
        ml_flags: 0x0001, // METH_VARARGS
        ml_doc: std::ptr::null(),
    }));
    // SAFETY: the definition is leaked and the interpreter is up.
    let function = unsafe { capi::PyCFunction_New(definition, std::ptr::null_mut()) };
    assert!(
        !function.is_null(),
        "Math: the getRegister function could not be created"
    );
    // SAFETY: the module is live and the function is a fresh reference.
    unsafe {
        capi::PyDict_SetItemString(
            capi::PyModule_GetDict(module),
            c"getRegister".as_ptr(),
            function,
        );
        capi::dec_ref(function);
    }
}

/// The fini-time teardown: leave the store lists and the register handles.
/// The register *values* stay, the statics they are -- the reference keeps
/// them across interpreter runs too (`script_math.cpp:4887-4889`).
pub(super) fn reset() {
    v4s()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clear();
    ms().lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clear();
    register_handles()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clear();
}

// -----------------------------------------------------------------------------
// Section: the type descriptions
// -----------------------------------------------------------------------------

/// The `Vector4Basic` description: `value` only -- the reference declares
/// no `pySet_value` (`script_math.cpp:3244-3253`), so the attribute is
/// read-only.
fn basic_spec() -> TypeSpec {
    TypeSpec::new(BASIC)
        .instance_size(std::mem::size_of::<BasicBody>())
        .attribute("value", get_basic_value, None)
}

/// The `Vector4Product` description.
fn product_spec() -> TypeSpec {
    TypeSpec::new(PRODUCT)
        .instance_size(std::mem::size_of::<ProductBody>())
        .attribute("value", get_product_value, None)
        .attribute("a", get_product_a, Some(set_product_a))
        .attribute("b", get_product_b, Some(set_product_b))
        .slot(capi::Py_tp_new, product_new as *mut std::os::raw::c_void)
}

/// The `Vector4Animation` description.
fn animation_spec() -> TypeSpec {
    TypeSpec::new(ANIMATION)
        .instance_size(std::mem::size_of::<AnimationBody>())
        .attribute("value", get_animation_value, None)
        .attribute(
            "keyframes",
            get_animation_keyframes,
            Some(set_animation_keyframes),
        )
        .attribute("time", get_animation_time, Some(set_animation_time))
        .attribute(
            "duration",
            get_animation_duration,
            Some(set_animation_duration),
        )
        .slot(capi::Py_tp_new, animation_new as *mut std::os::raw::c_void)
}

/// The `Vector4Morph` description.
fn morph_spec() -> TypeSpec {
    TypeSpec::new(MORPH)
        .instance_size(std::mem::size_of::<MorphBody>())
        .attribute("value", get_morph_value, None)
        .attribute("target", get_morph_target, Some(set_morph_target))
        .attribute("duration", get_morph_duration, Some(set_morph_duration))
        .attribute("time", get_morph_time, Some(set_morph_time))
        .slot(capi::Py_tp_new, morph_new as *mut std::os::raw::c_void)
}

/// The `Vector4Translation` description.
fn translation_spec() -> TypeSpec {
    TypeSpec::new(TRANSLATION)
        .instance_size(std::mem::size_of::<TranslationBody>())
        .attribute("value", get_translation_value, None)
        .attribute(
            "source",
            get_translation_source,
            Some(set_translation_source),
        )
        .slot(
            capi::Py_tp_new,
            translation_new as *mut std::os::raw::c_void,
        )
}

/// The `Vector4Distance` description.
fn distance_spec() -> TypeSpec {
    TypeSpec::new(DISTANCE)
        .instance_size(std::mem::size_of::<DistanceBody>())
        .attribute("value", get_distance_value, None)
        .attribute("a", get_distance_a, Some(set_distance_a))
        .attribute("b", get_distance_b, Some(set_distance_b))
        .slot(capi::Py_tp_new, distance_new as *mut std::os::raw::c_void)
}

/// The `Vector4LFO` description.
fn lfo_spec() -> TypeSpec {
    TypeSpec::new(LFO)
        .instance_size(std::mem::size_of::<LfoBody>())
        .attribute("value", get_lfo_value, None)
        .attribute("period", get_lfo_period, Some(set_lfo_period))
        .attribute("amplitude", get_lfo_amplitude, Some(set_lfo_amplitude))
        .attribute("phase", get_lfo_phase, Some(set_lfo_phase))
        .attribute("waveform", get_lfo_waveform, Some(set_lfo_waveform))
        .attribute("time", get_lfo_time, Some(set_lfo_time))
        .slot(capi::Py_tp_new, lfo_new as *mut std::os::raw::c_void)
}

/// The `Vector4MatrixAdaptor` description.
fn adaptor_spec() -> TypeSpec {
    TypeSpec::new(ADAPTOR)
        .instance_size(std::mem::size_of::<AdaptorBody>())
        .attribute("value", get_adaptor_value, None)
        .attribute("source", get_adaptor_source, Some(set_adaptor_source))
        .attribute("position", get_adaptor_position, Some(set_adaptor_position))
        .attribute("style", get_adaptor_style, Some(set_adaptor_style))
        .slot(capi::Py_tp_new, adaptor_new as *mut std::os::raw::c_void)
}

/// The `Vector4Swizzle` description.
fn swizzle_spec() -> TypeSpec {
    TypeSpec::new(SWIZZLE)
        .instance_size(std::mem::size_of::<SwizzleBody>())
        .attribute("value", get_swizzle_value, None)
        .attribute("x", get_swizzle_x, Some(set_swizzle_x))
        .attribute("y", get_swizzle_y, Some(set_swizzle_y))
        .attribute("z", get_swizzle_z, Some(set_swizzle_z))
        .attribute("w", get_swizzle_w, Some(set_swizzle_w))
        .slot(capi::Py_tp_new, swizzle_new as *mut std::os::raw::c_void)
}

/// The `Vector4Combiner` description.
fn combiner_spec() -> TypeSpec {
    TypeSpec::new(COMBINER)
        .instance_size(std::mem::size_of::<CombinerBody>())
        .attribute("value", get_combiner_value, None)
        .attribute("fn", get_combiner_fn, Some(set_combiner_fn))
        .attribute("a", get_combiner_a, Some(set_combiner_a))
        .attribute("b", get_combiner_b, Some(set_combiner_b))
        .slot(capi::Py_tp_new, combiner_new as *mut std::os::raw::c_void)
}

/// The `Vector4Register` description: no attributes, no factory -- the
/// type exists for the handles `getRegister` builds.
fn register_spec() -> TypeSpec {
    TypeSpec::new(REGISTER)
        .instance_size(std::mem::size_of::<RegisterBody>())
        .attribute("value", get_register_value, None)
}

/// The `Vector4Shader` description: `addOp` only.
fn shader_spec() -> TypeSpec {
    TypeSpec::new(SHADER)
        .instance_size(std::mem::size_of::<ShaderBody>())
        .attribute("value", get_shader_value, None)
        .method("addOp", method_shader_add_op)
        .slot(capi::Py_tp_new, shader_new as *mut std::os::raw::c_void)
}

/// The `MatrixProduct` description.
fn matrix_product_spec() -> TypeSpec {
    TypeSpec::new(MATRIX_PRODUCT)
        .instance_size(std::mem::size_of::<MatrixProductBody>())
        .attribute("a", get_matrix_product_a, Some(set_matrix_product_a))
        .attribute("b", get_matrix_product_b, Some(set_matrix_product_b))
        .slot(
            capi::Py_tp_new,
            matrix_product_new as *mut std::os::raw::c_void,
        )
}

/// The `MatrixInverse` description.
fn matrix_inverse_spec() -> TypeSpec {
    TypeSpec::new(MATRIX_INVERSE)
        .instance_size(std::mem::size_of::<MatrixInverseBody>())
        .attribute(
            "source",
            get_matrix_inverse_source,
            Some(set_matrix_inverse_source),
        )
        .slot(
            capi::Py_tp_new,
            matrix_inverse_new as *mut std::os::raw::c_void,
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The componentwise arithmetic: the product, the quotient with a zero
    /// divisor cell, the dot spreading its scalar.
    #[test]
    fn the_componentwise_operators_follow_the_reference() {
        let a = [1.0, 2.0, 0.5, 0.0];
        let b = [2.0, 2.0, 2.0, 2.0];
        assert_eq!(parallel_multiply4(a, b), [2.0, 4.0, 1.0, 0.0]);
        assert_eq!(add4(a, b), [3.0, 4.0, 2.5, 2.0]);
        assert_eq!(subtract4(b, a), [1.0, 0.0, 1.5, 2.0]);
        assert_eq!(dot4([1.0, 2.0, 3.0, 4.0], [1.0, 1.0, 1.0, 1.0]), 10.0);
        let q = divide4([1.0, 2.0, 4.0, 8.0], [2.0, 0.0, 4.0, 1.0]);
        assert_eq!(q[0], 0.5);
        assert!(q[1].is_infinite());
        assert_eq!(q[2], 1.0);
        assert_eq!(q[3], 8.0);
    }

    /// `XPVec4Lerp` at the ends and the middle.
    #[test]
    fn the_lerp_walks_between_its_ends() {
        let a = [0.0, 0.0, 0.0, 0.0];
        let b = [4.0, 4.0, 4.0, 4.0];
        assert_eq!(lerp4(a, b, 0.0), a);
        assert_eq!(lerp4(a, b, 1.0), b);
        assert_eq!(lerp4(a, b, 0.5), [2.0; 4]);
        assert_eq!(lerp4(a, b, 0.25), [1.0; 4]);
    }

    /// The Animation walk: before the first, between two, after the last,
    /// and the wrap of the tick clock.
    #[test]
    fn the_animation_walk_answers_nearest_and_lerped_keyframes() {
        let mut body = AnimationBody {
            keyframes: Vec::new(),
            time: 0.0,
            duration: 1.0,
        };
        let mut out = [0.0_f32; 4];
        body.output(&mut out);
        assert_eq!(out, [0.0; 4]);

        // A body whose slots answer constants: two keyframes at 0.25 and
        // 0.75, both Basic providers built through the store-free path is
        // not available without the interpreter, so drive the walk with
        // the slot addresses resolved through field_output -- empty here.
        // The times alone decide the branch structure.
        body.keyframes = vec![(0.25, None), (0.75, None)];
        body.time = 0.1;
        body.output(&mut out); // before the first: first slot (None: zero)
        assert_eq!(out, [0.0; 4]);
        body.time = 0.9;
        body.output(&mut out); // after the last: last slot (None: zero)
        assert_eq!(out, [0.0; 4]);
        body.time = 0.5;
        body.output(&mut out); // between: the lerp of two empty slots
        assert_eq!(out, [0.0; 4]);

        // The tick wraps at the duration floor.
        body.time = 0.9995;
        body.duration = 1.0;
        body.tick(0.001);
        assert!((body.time - 0.0005).abs() < 1e-6, "{}", body.time);
        body.duration = 0.0005; // below the floor
        body.time = 1.0;
        body.tick(1.0);
        // (1.0 + 1.0) % max(0.0005, 0.001) = 2.0 % 0.001
        assert!((body.time - (2.0 % 0.001)).abs() < 1e-9, "{}", body.time);
    }

    /// The Morph clock: the setter clamps to the duration, the duration
    /// setter carries the progress ratio, the target setter snapshots the
    /// current output.
    #[test]
    fn the_morph_clock_clamps_and_carries() {
        let mut body = MorphBody {
            target: [4.0; 4],
            old: [0.0; 4],
            time: 0.0,
            duration: 1.0,
        };
        let mut out = [0.0_f32; 4];
        body.output(&mut out);
        assert_eq!(out, [0.0; 4]);

        body.time = 5.0;
        body.set_time(5.0);
        assert!((body.time - 1.0).abs() < 1e-7, "{}", body.time);
        body.set_time(0.0);
        assert!((body.time - 0.0001).abs() < 1e-9, "{}", body.time);

        // The duration setter carries the ratio: halfway at duration 1.0
        // stays halfway at duration 2.0.
        body.duration = 1.0;
        body.set_time(0.5);
        body.set_duration(2.0);
        assert!((body.time - 1.0).abs() < 1e-6, "{}", body.time);
        body.output(&mut out);
        assert_eq!(out, [2.0; 4]);

        // The target setter freezes the current output as the old value.
        body.set_duration(1.0);
        body.set_time(1.0);
        body.set_target([8.0, 8.0, 8.0, 8.0]);
        assert_eq!(body.old, [4.0; 4]);
        assert!((body.time - 0.0).abs() < 1e-9);
        body.output(&mut out);
        assert_eq!(out, [4.0; 4]);
    }

    /// The four LFO waveforms at their named landmarks.
    #[test]
    fn the_lfo_waveforms_follow_their_shapes() {
        let base = |waveform: usize| LfoBody {
            waveform,
            period: 1.0,
            amplitude: 2.0,
            phase: 0.0,
            time: 0.0,
            parity: 0,
        };
        let mut out = [0.0_f32; 4];

        // SQUARE: positive half amplitude, negative half zero.
        let mut body = base(0);
        body.time = 0.75;
        body.output(&mut out);
        assert_eq!(out, [2.0; 4]);
        body.time = 0.25;
        body.output(&mut out);
        assert_eq!(out, [0.0; 4]);
        // A silent period.
        body.period = 0.0001;
        body.output(&mut out);
        assert_eq!(out, [0.0; 4]);

        // TRIANGLE: the raw ramp is 0.75 at both landmarks (rising over
        // the positive half, folded over the negative one), scaled by the
        // amplitude.
        let mut body = base(1);
        body.time = 0.75;
        body.output(&mut out);
        assert!((out[0] - 1.5).abs() < 1e-6, "{}", out[0]);
        body.time = 0.25;
        body.output(&mut out);
        assert!((out[0] - 1.5).abs() < 1e-6, "{}", out[0]);

        // SINE: the hump rides 0..1, so the phase-0 read is half the
        // amplitude and the quarter period is the peak; the clock never
        // folds.
        let mut body = base(2);
        body.output(&mut out);
        assert!((out[0] - 1.0).abs() < 1e-6, "{}", out[0]);
        body.time = 0.25;
        body.output(&mut out);
        assert!((out[0] - 2.0).abs() < 1e-5, "{}", out[0]);
        let before = body.time;
        body.output(&mut out);
        assert_eq!(body.time, before);

        // SAWTOOTH: ramps 0..1 across the period.
        let mut body = base(3);
        body.time = 0.5;
        body.output(&mut out);
        assert!((out[0] - 1.0).abs() < 1e-6, "{}", out[0]);
        // The tick advances the clock and flips the parity.
        let parity = body.parity;
        body.tick(0.1);
        assert!((body.time - 0.6).abs() < 1e-6);
        assert_eq!(body.parity, parity.wrapping_add(1));
    }

    /// The shader opcode table over fixed inputs.
    #[test]
    fn the_shader_opcodes_evaluate_their_arithmetic() {
        let a = [1.0, 2.0, 3.0, 4.0];
        let b = [2.0, 2.0, 0.0, 4.0];
        assert_eq!(apply_shader_op(0, a, b), Some(a));
        assert_eq!(apply_shader_op(1, [2.0; 4], b), Some([0.5; 4]));
        assert_eq!(apply_shader_op(2, [1.0; 4], b), Some([0.5; 4]));
        assert_eq!(apply_shader_op(3, [1.0; 4], b), Some([0.0; 4]));
        assert_eq!(apply_shader_op(4, a, b), Some([2.0, 4.0, 0.0, 16.0]));
        assert_eq!(apply_shader_op(6, a, b), Some([3.0, 4.0, 3.0, 8.0]));
        assert_eq!(apply_shader_op(7, a, b), Some([-1.0, 0.0, 3.0, 0.0]));
        assert_eq!(apply_shader_op(8, a, b), Some([22.0; 4]));
        assert_eq!(apply_shader_op(9, a, b), Some([1.0, 2.0, 0.0, 4.0]));
        assert_eq!(apply_shader_op(10, a, b), Some([2.0, 2.0, 3.0, 4.0]));
        // SGE compares i0.x against every component of i1; SLT the negation.
        let x = [1.0, 0.0, 0.0, 0.0];
        assert_eq!(apply_shader_op(11, [1.0; 4], x), Some([1.0, 1.0, 1.0, 1.0]));
        assert_eq!(apply_shader_op(12, [1.0; 4], x), Some([0.0, 0.0, 0.0, 0.0]));
        // An opcode the reference switch has no case for writes nothing,
        // so the output cell keeps whatever held it.
        assert_eq!(apply_shader_op(99, a, b), None);
    }

    /// The enum sentences: the `", or "` before the last name.
    #[test]
    fn the_enum_sentences_list_their_names() {
        assert_eq!(
            enum_sentence("waveform", WAVEFORMS),
            "waveform must be set to one of SQUARE, TRIANGLE, SINE, or SAWTOOTH"
        );
        assert_eq!(
            enum_sentence("fn", FUNCTIONS),
            "fn must be set to one of MULTIPLY, DIVIDE, ADD, SUBTRACT, DOT, MIN, or MAX"
        );
        assert_eq!(
            enum_sentence("style", STYLES),
            "style must be set to one of XYZ_SCALE, XY_SCALE, X_ROTATE, \
             Y_ROTATE, Z_ROTATE, LOOKAT, or LOOKAT_SCALEZ"
        );
    }
}
