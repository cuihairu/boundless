//! The `ResMgr` module: the script faces of the resource engine -- the
//! module functions of `res_mgr_script.cpp`, the `PyDataSection` wrapper of
//! `py_data_section.cpp` and the deprecated `ResourceTable`/`ResTblStruct`
//! pair of `resource_table.cpp`.
//!
//! The reference builds the module out of static links at init time: one
//! `PyModuleMethodLink` per function joins the `ResMgr` module
//! (`script.cpp:1409-1453`), `PY_MODULE_ATTRIBUTE` adds `root`
//! (`res_mgr_script.cpp:353-355`) and the `ResMgr_token` of
//! `PyDataSection_token | ResourceTable_token` registers the classes
//! (`res_mgr_script.cpp:341-346`). Seven functions, the `ResourceTable`
//! factory and the three classes are unconditional; the editor trio
//! (`remove`/`rename`/`copy`) sits behind `#ifdef EDITOR_ENABLED`
//! (`res_mgr_script.cpp:166-260`), which the port renders as a runtime
//! role test: an `editor` component carries the trio, every other
//! component -- and the nameless default -- does not.
//!
//! The wrapper model: the reference hands out `DataSectionPtr` handles into
//! one shared tree, so two opens of the same id alias and every write is
//! visible everywhere. The port keeps that with one `Rc<RefCell<Tree>>` per
//! opened resource -- the census that `DataSectionCensus` provided
//! (`data_section_census.cpp`) maps the resource id to its tree, and a
//! `PyDataSection` wrapper is a root plus a slash path resolved at call
//! time. `save` writes the whole tree back through the engine under the
//! origin id.
//!
//! Deltas kept out or accepted, all visible from script:
//! - `createSectionFromString` copies the parsed section into the parent
//!   but returns the DETACHED parse, not the copy that landed
//!   (`py_data_section.cpp:1803-1808`) -- the defect is kept.
//! - `deleteSection` with a wrapper deletes the first child carrying the
//!   target's name; the reference matches by section identity
//!   (`:1871-1875`).
//! - `child(i)`, `values`, `items` and every path answer resolve to the
//!   FIRST child carrying the name; duplicate tags collapse, where the
//!   reference addresses children by position.
//! - A wrapper whose node was deleted reads type defaults and drops
//!   writes; the reference keeps the detached node alive to the last
//!   handle. Its `child` raises the "has disappeared" `EnvironmentError`
//!   (`:1979-1985`), which the orphaned port wrapper raises with an empty
//!   name.
//! - Undotted `openSection(makeNew=true)` refuses with the reference's
//!   `ValueError`: the engine has no `DirSection` yet, so every directory
//!   create takes the "Could not make new section" arm. A directory path
//!   without `makeNew` answers `None`.
//! - `root` is an empty in-memory section, not the engine's directory
//!   tree -- the dir-section task owns the difference.
//! - `writeMatrix` and the `asMatrix` setter accept a `Math.Matrix`; the
//!   reference's `MatrixProvider::Check` also matches the provider types.
//! - `localise` answers its input string unchanged: no localisation data
//!   is in scope, so `LocaliseUTF8`'s table lookup always falls back.
//! - The per-item `write*s` failure raises `TypeError` with the reference's
//!   sentence; the reference passes the args tuple to `PyErr_Format` in
//!   place of an exception type (`py_data_section.cpp:815`) and would
//!   crash a 3.x interpreter.
//! - The `ResourceTable` census is keyed by resource id; the reference's
//!   census compares wrapped section pointers (the `RTSCompare` set,
//!   `resource_table.cpp:569-578`), which the port's cloned trees cannot
//!   identity-match.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::os::raw::{c_char, c_int, c_void};
use std::rc::Rc;
use std::sync::{Mutex, OnceLock};

use foundation::math::{Vector2, Vector3, Vector4};
use resources::base64;
use resources::bwresource::BwResource;
use resources::datasection::{DataSection, parse as parse_xml};

use crate::binding::{self, TypeSpec};
use crate::capi;
use crate::math::matrix::{MatrixValue, matrix_object, matrix_of};
use crate::math::vector::{parse_vector_like, vector_object};
use crate::math::{
    arg_count_between_sentence, arg_count_sentence, body, bool_object, own_raw, raise_type_error,
};
use crate::pyref::PyRef;

/// The type names the reference's `s_type_` objects carry after the
/// `PyFactoryMethodLink` rewrite (`py_factory_method_link.cpp:37-80`):
/// the link adds each type to the module under its bare name --
/// `DataSection` (`PY_FACTORY_NAMED`, `py_data_section.cpp:360`) and
/// `ResourceTable` (`PY_FACTORY`, `resource_table.cpp:666`) -- and rewrites
/// `tp_name` to the qualified form the error texts quote. `ResTblStruct`
/// is never linked: it only ever exists as a `ResourceTable.value`
/// instance, so its `tp_name` stays the class name.
const DATA_SECTION_NAME: &str = "ResMgr.DataSection";
const TABLE_NAME: &str = "ResMgr.ResourceTable";
const STRUCT_NAME: &str = "ResTblStruct";

// ---------------------------------------------------------------------------
// The wrapper model: one shared tree per opened resource.
// ---------------------------------------------------------------------------

/// One opened resource's live tree plus where it came from: `origin` is the
/// resource id `save` writes back to, and `None` for sections built in
/// memory (`PyDataSection("name")`, `root`, detached copies).
struct Tree {
    data: DataSection,
    origin: Option<String>,
}

/// The shared handle every wrapper of one resource holds.
type TreeHandle = Rc<RefCell<Tree>>;

/// A `PyDataSection` instance body: the shared tree plus the slash path
/// from the root, resolved at call time so aliasing stays live.
struct SectionBody {
    root: TreeHandle,
    path: String,
}

/// The values a `ResTblStruct` reads from: the tree and the child-index
/// path of the node inside it -- the wrapped `value` child of the owning
/// table's section (`resource_table.cpp:364-377`). Duplicate tags need
/// positions, not names. A missing entry (`indices` pointing past the
/// tree) is the reference's detached node, which reads as empty.
struct StructValues {
    root: TreeHandle,
    indices: Vec<usize>,
}

/// Walk a tree by child-index positions, the way duplicate-tag entries
/// are addressed.
fn indexed_node<'a>(data: &'a DataSection, indices: &[usize]) -> Option<&'a DataSection> {
    let mut node = data;
    for &index in indices {
        node = node.open_child(index)?;
    }
    Some(node)
}

/// One `ResTblStruct` instance body: the values it reads from and the
/// parent chain `get` falls back through (`resource_table.cpp:32-45`) --
/// the parent is another `ResTblStruct` instance, the reference's
/// `ResTblStructPtr pParent_`.
struct StructBody {
    values: Option<StructValues>,
    parent: Option<PyRef>,
}

/// A `ResourceTable` instance body: the tree it reads from, the
/// child-index path of the section it wraps (empty for the root table --
/// the factory's opened resource), its value struct, the keyed sub-tables
/// built at construction (`keyedEntries_`, `resource_table.cpp:379-388`)
/// and the linked update callables (`links_`, a set in the reference).
struct TableBody {
    root: TreeHandle,
    indices: Vec<usize>,
    value: PyRef,
    entries: Vec<PyRef>,
    index: u32,
    links: Vec<PyRef>,
}

/// The module state: the engine the faces run on, the tree census keyed by
/// resource id and the `ResourceTable` census keyed the same way
/// (`s_census`). `Rc`/`RefCell` are not `Send`, but every touch is on the
/// interpreter thread under the GIL -- the same shape as the timer store.
struct Store {
    resource: BwResource,
    sections: BTreeMap<String, TreeHandle>,
    tables: BTreeMap<String, PyRef>,
}

unsafe impl Send for Store {}

static STORE: OnceLock<Mutex<Store>> = OnceLock::new();

fn store() -> &'static Mutex<Store> {
    STORE.get_or_init(|| {
        Mutex::new(Store {
            resource: BwResource::new(),
            sections: BTreeMap::new(),
            tables: BTreeMap::new(),
        })
    })
}

/// `BWResource::init`: lay the search paths down before the interpreter
/// comes up, the way a host's `main` does. A later [`install`] picks the
/// store up; without this call the faces run on an empty engine.
pub fn init_paths(paths: &[&str]) {
    let mut store_state = store()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    *store_state = Store {
        resource: BwResource::init_paths(paths),
        sections: BTreeMap::new(),
        tables: BTreeMap::new(),
    };
}

// ---------------------------------------------------------------------------
// Type and teardown registries: the self-contained copies of the math
// module's pattern, keyed the same way.
// ---------------------------------------------------------------------------

/// The created type objects, by Python-visible name.
static TYPES: OnceLock<Mutex<BTreeMap<String, usize>>> = OnceLock::new();

fn types() -> &'static Mutex<BTreeMap<String, usize>> {
    TYPES.get_or_init(|| Mutex::new(BTreeMap::new()))
}

/// Record a created type under its Python name.
fn remember_type(name: &str, object: &PyRef) {
    types()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .insert(name.to_owned(), object.as_ptr() as usize);
}

/// The address of a created type object, for engine-side instantiation.
fn type_address(name: &str) -> Option<usize> {
    types()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(name)
        .copied()
}

/// Whether `object` is exactly the type created under `name`: the
/// `Class::Check` of the reference (`pyobject_plus.hpp:118-123`), exact
/// identity because nothing here sets `Py_TPFLAGS_BASETYPE`.
///
/// # Safety
///
/// `object` must be a borrowed object the interpreter handed out, or null.
unsafe fn check_type(object: *mut capi::PyObject, name: &str) -> bool {
    if object.is_null() {
        return false;
    }
    match type_address(name) {
        Some(address) => (unsafe { capi::Py_TYPE(object) }) as usize == address,
        None => false,
    }
}

/// One instance teardown: drops the Rust body the instance carries.
type Teardown = unsafe fn(*mut capi::PyObject);

/// The teardown handlers, keyed on the type object's address.
static TEARDOWNS: OnceLock<Mutex<BTreeMap<usize, Teardown>>> = OnceLock::new();

fn teardowns() -> &'static Mutex<BTreeMap<usize, Teardown>> {
    TEARDOWNS.get_or_init(|| Mutex::new(BTreeMap::new()))
}

/// Register a body type for a created type object.
fn register_teardown<T>(address: usize) {
    teardowns()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .insert(address, drop_body::<T>);
}

/// The `drop_in_place` step for one body type.
unsafe fn drop_body<T>(object: *mut capi::PyObject) {
    // SAFETY: the dealloc runs exactly once per instance and the caller
    // guarantees the instance shape.
    unsafe { std::ptr::drop_in_place(body::<T>(object)) };
}

/// The `tp_dealloc` of the body-carrying types: teardown, free, drop the
/// heap type's own reference.
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
        // SAFETY: the teardown is registered for exactly this type.
        unsafe { teardown(object) };
    }
    // SAFETY: the block came from `PyType_GenericAlloc`'s allocator.
    unsafe { capi::PyObject_Free(object.cast()) };
    // SAFETY: the type object is a heap type; the instance held one
    // reference of it.
    unsafe { capi::dec_ref(type_.cast()) };
}

/// Attach the body-carrying dealloc to a spec.
fn with_dealloc(spec: TypeSpec) -> TypeSpec {
    spec.slot(capi::Py_tp_dealloc, native_dealloc as *mut c_void)
}

// ---------------------------------------------------------------------------
// Error helpers: the exception texts the faces raise.
// ---------------------------------------------------------------------------

/// `PyErr_SetString` on a chosen global exception, with a Rust message.
fn raise(exception: *mut capi::PyObject, message: &str) {
    // SAFETY: the message is a fresh unicode object.
    unsafe {
        let rendered = capi::unicode_from_bytes(message.as_bytes());
        capi::PyErr_SetObject(exception, rendered);
        capi::dec_ref(rendered);
    }
}

/// The `raise` step on the `ValueError` global, for the open section
/// refusals.
fn raise_value_error(message: &str) {
    unsafe { raise(capi::PyExc_ValueError, message) };
}

/// The `raise` step on the `IOError` global, for the file system and save
/// refusals.
fn raise_io_error(message: &str) {
    unsafe { raise(capi::PyExc_IOError, message) };
}

/// The `raise` step on the `IndexError` global, for the subscript and
/// child walks.
fn raise_index_error(message: &str) {
    unsafe { raise(capi::PyExc_IndexError, message) };
}

/// The `raise` step on the `EnvironmentError` global, for the vanished
/// child handles.
fn raise_environment_error(message: &str) {
    unsafe { raise(capi::PyExc_EnvironmentError, message) };
}

/// The `raise` step on the `SystemError` global, for the internal sequence
/// walk.
fn raise_system_error(message: &str) {
    unsafe { raise(capi::PyExc_SystemError, message) };
}

/// The `raise` step on the `AttributeError` global.
fn raise_attribute_error(message: &str) {
    unsafe { raise(capi::PyExc_AttributeError, message) };
}

/// The `argCountError` of the auto-parsed faces, from the expectation
/// list: one accepted type is the fixed sentence, more is the
/// between-sentence (`script.cpp:2520-2568`).
fn arg_count_error(name: &str, expected: &[&str]) {
    if expected.len() == 1 {
        raise_type_error(&arg_count_sentence(name, expected));
    } else {
        raise_type_error(&arg_count_between_sentence(name, 1, expected));
    }
}

/// The wrong-type `varName` the auto machinery passes to `Script::setData`:
/// `"() argument N"`, with `" optionally"` on the optional tail
/// (`script.hpp:1117-1131`). `number` is the reference's 1-based argument
/// number (`"() argument " #N` of the auto macros).
fn arg_var_name(number: usize, optional: bool) -> String {
    if optional {
        format!("() argument {number} optionally")
    } else {
        format!("() argument {number}")
    }
}

/// The wrong-type failure of the argument at tuple index `position`: the
/// `Script::setData` sentence with the auto `varName` (`position + 1` --
/// the reference's argument numbers start at 1).
fn raise_arg_type(type_sentence: &str, position: usize, optional: bool) {
    raise_type_error(&format!(
        "{} {type_sentence}",
        arg_var_name(position + 1, optional)
    ));
}

/// The `NZARG` sentence: a `PyObjectPtr` argument handed `None` is
/// refused -- `None` is the zero value (`script.hpp:1128-1131`); the
/// argument number is 1-based as above.
fn raise_arg_cannot_be_none(position: usize) {
    raise_type_error(&format!("() argument {} cannot be None", position + 1));
}

/// Parse one string argument of an auto-parsed call: the `BW::string`
/// coercion with the auto `varName`, `None` on refusal (error pending).
///
/// # Safety
///
/// The interpreter holds the GIL and `args` is the arguments tuple.
unsafe fn string_arg(args: *mut capi::PyObject, position: usize, optional: bool) -> Option<String> {
    let object = unsafe { capi::PyTuple_GetItem(args, position as capi::PySsize) };
    if unsafe { capi::py_str_check(object) } {
        // SAFETY: the check above guarantees a str object.
        return Some(unsafe { capi::to_string(object) });
    }
    raise_arg_type("must be set to a string.", position, optional);
    None
}

/// Parse one bool argument: the `bool` coercion with the auto `varName`.
///
/// # Safety
///
/// The interpreter holds the GIL and `args` is the arguments tuple.
unsafe fn bool_arg(args: *mut capi::PyObject, position: usize, optional: bool) -> Option<bool> {
    let object = unsafe { capi::PyTuple_GetItem(args, position as capi::PySsize) };
    if unsafe { capi::py_long_check(object) } {
        // SAFETY: the check above guarantees an int object.
        let value = unsafe { capi::PyLong_AsLong(object) };
        return Some(value != 0);
    }
    if unsafe { capi::py_str_check(object) } {
        // SAFETY: the check above guarantees a str object.
        let text = unsafe { capi::to_string(object) };
        if text.eq_ignore_ascii_case("true") {
            return Some(true);
        }
        if text.eq_ignore_ascii_case("false") {
            return Some(false);
        }
    }
    raise_arg_type("must be set to a bool", position, optional);
    None
}

/// Parse one int argument: the `int` coercion with the auto `varName`; an
/// overflow clears its error and falls to the refusal
/// (`script.cpp:1623`).
///
/// # Safety
///
/// The interpreter holds the GIL and `args` is the arguments tuple.
unsafe fn int_arg(args: *mut capi::PyObject, position: usize) -> Option<i32> {
    let object = unsafe { capi::PyTuple_GetItem(args, position as capi::PySsize) };
    if unsafe { capi::py_long_check(object) } {
        // SAFETY: the check above guarantees an int object.
        let value = unsafe { capi::PyLong_AsLong(object) };
        let overflow = unsafe { capi::PyErr_Occurred() }.is_null();
        if overflow {
            let narrowed = value as i32;
            if value as i64 == narrowed as i64 {
                return Some(narrowed);
            }
        } else {
            // SAFETY: the conversion raised an overflow.
            unsafe { capi::clear_error() };
        }
    }
    raise_arg_type("must be set to an int", position, false);
    None
}

/// Parse one `PyObjectPtr` argument: any object except `None`, the
/// reference's `NZARG` refusal.
///
/// # Safety
///
/// The interpreter holds the GIL and `args` is the arguments tuple.
unsafe fn object_arg(args: *mut capi::PyObject, position: usize) -> Option<*mut capi::PyObject> {
    let object = unsafe { capi::PyTuple_GetItem(args, position as capi::PySsize) };
    if object == capi::py_none() {
        raise_arg_cannot_be_none(position);
        return None;
    }
    Some(object)
}

/// Parse one callable argument, the `link`/factory shape: any object
/// except `None`, then the callable wall.
///
/// # Safety
///
/// The interpreter holds the GIL and `args` is the arguments tuple.
unsafe fn callable_arg(args: *mut capi::PyObject, position: usize) -> Option<*mut capi::PyObject> {
    let object = unsafe { object_arg(args, position)? };
    if unsafe { capi::PyCallable_Check(object) } == 0 {
        raise_type_error("ResourceTable.link() arg must be callable");
        return None;
    }
    Some(object)
}

/// `Script::call` (`script.cpp`): call with the arguments, and on failure
/// print and clear -- the timers module's port of the same helper.
///
/// # Safety
///
/// The interpreter holds the GIL and both objects are live.
unsafe fn call_and_report(function: *mut capi::PyObject, args: *mut capi::PyObject) {
    // SAFETY: the interpreter holds the GIL and both objects are live.
    let result = unsafe { capi::PyObject_CallObject(function, args) };
    if result.is_null() {
        // SAFETY: the error is pending.
        unsafe {
            capi::PyErr_PrintEx(0);
            capi::clear_error();
        }
    } else {
        // SAFETY: the answer reference is this call's to drop.
        unsafe { capi::dec_ref(result) };
    }
}

// ---------------------------------------------------------------------------
// Simple object builders.
// ---------------------------------------------------------------------------

/// A fresh `str` object from Rust text.
fn string_object(text: &str) -> *mut capi::PyObject {
    // SAFETY: creating a unicode object is always safe while the
    // interpreter is up.
    unsafe { capi::unicode_from_bytes(text.as_bytes()) }
}

/// A fresh `int` object.
fn int_object(value: i64) -> *mut capi::PyObject {
    // SAFETY: as above.
    unsafe { capi::PyLong_FromLong(value) }
}

/// A fresh `float` object.
fn float_object(value: f64) -> *mut capi::PyObject {
    // SAFETY: as above.
    unsafe { capi::PyFloat_FromDouble(value) }
}

/// A fresh `bool` object, the `PyBool_FromLong` of the predicate faces.
fn boolean_object(value: bool) -> *mut capi::PyObject {
    bool_object(value)
}

// ---------------------------------------------------------------------------
// The `bw_util` path helpers the module functions use.
// ---------------------------------------------------------------------------

/// `BWUtil::normalisePath` (`bw_util.cpp:34-84`): tokenise on both
/// separators, drop empty and "." tokens, fold `x/..` pairs (a leading
/// `..` stays), join with `/`, keep a leading separator, and answer `.`
/// when nothing is left.
fn normalise_path(path: &str) -> String {
    let mut tokens: Vec<&str> = Vec::new();
    for token in path.split(['/', '\\']) {
        match token {
            "" | "." => {}
            ".." => {
                if tokens.pop().is_none() {
                    // A leading `..` has nothing to fold against.
                    tokens.push("..");
                }
            }
            other => tokens.push(other),
        }
    }
    if tokens.is_empty() {
        return ".".to_owned();
    }
    let prefix = if path.starts_with('/') || path.starts_with('\\') {
        "/"
    } else {
        ""
    };
    format!("{prefix}{}", tokens.join("/"))
}

/// `BWUtil::isAbsolutePath` (`bw_util.cpp:86`): a leading separator or a
/// `X:` drive prefix.
fn is_absolute_path(path: &str) -> bool {
    if path.starts_with('/') || path.starts_with('\\') {
        return true;
    }
    let bytes = path.as_bytes();
    bytes.len() >= 2 && bytes[1] == b':' && bytes[0].is_ascii_alphabetic()
}

// ---------------------------------------------------------------------------
// Tree walking.
// ---------------------------------------------------------------------------

/// A strict walk down a `'/'`-separated tag path, first matches only: the
/// mutable half of `openSection` with `makeNewSection = false`. `None`
/// when any component misses.
fn walk_mut<'a>(data: &'a mut DataSection, path: &str) -> Option<&'a mut DataSection> {
    if path.is_empty() {
        return Some(data);
    }
    let mut node = data;
    for tag in path.split('/') {
        let mut index = None;
        for position in 0..node.count_children() {
            if node
                .open_child(position)
                .is_some_and(|child| child.section_name() == tag)
            {
                index = Some(position);
                break;
            }
        }
        node = node.child_mut(index?)?;
    }
    Some(node)
}

/// The immutable read walk: the engine's own `openSection`, which carries
/// the reference's slash quirks.
fn walk_read<'a>(data: &'a DataSection, path: &str) -> Option<&'a DataSection> {
    data.open_section(path)
}

/// The `splitTagPath` shape the write*s use: the parent of the leaf tag
/// plus the leaf. With `make_new` the parent chain is created, the way the
/// write engine calls do.
fn split_tag_path<'a>(
    section: &'a mut DataSection,
    path: &str,
    make_new: bool,
) -> Option<(&'a mut DataSection, String)> {
    match path.rsplit_once('/') {
        Some((base, leaf)) => {
            let parent = if make_new {
                section.open_or_make_section(base)?
            } else {
                walk_mut(section, base)?
            };
            Some((parent, leaf.to_owned()))
        }
        None => Some((section, path.to_owned())),
    }
}

/// The immutable split for the read*s: the parent node and the leaf tag.
fn split_tag_read<'a>(section: &'a DataSection, path: &str) -> Option<(&'a DataSection, String)> {
    match path.rsplit_once('/') {
        Some((base, leaf)) => Some((section.open_section(base)?, leaf.to_owned())),
        None => Some((section, path.to_owned())),
    }
}

/// Clone the parts of a section wrapper out of the instance pointer.
///
/// # Safety
///
/// `object` must be an instance of the [`DATA_SECTION_NAME`] type this
/// module created.
unsafe fn section_parts(object: *mut capi::PyObject) -> (TreeHandle, String) {
    // SAFETY: the caller guarantees the instance shape.
    let body = unsafe { body::<SectionBody>(object) };
    // SAFETY: the instance outlives the call; the interpreter holds it.
    unsafe { ((*body).root.clone(), (*body).path.clone()) }
}

/// The wrapper the interpreter owns for `(root, path)`: allocate through
/// the created type and fill the body.
///
/// # Safety
///
/// The `PyDataSection` type must be installed and the caller owns the GIL.
unsafe fn section_object(root: TreeHandle, path: String) -> PyRef {
    let address = type_address(DATA_SECTION_NAME).expect("the data section type is installed");
    let type_object = PyRef::borrowed(address as *mut capi::PyObject);
    // SAFETY: the type is one of this module's, and the interpreter is up.
    let instance = unsafe { binding::instantiate(&type_object) }
        .expect("the section instance allocation cannot fail");
    // SAFETY: the instance is the fresh allocation this call owns. The
    // body region `PyType_GenericAlloc` handed back is uninitialised, so
    // the fields are written in one shot with `ptr::write` -- assigning
    // field by field would drop the garbage that already sits there.
    unsafe {
        std::ptr::write(
            body::<SectionBody>(instance.as_ptr()),
            SectionBody { root, path },
        );
    }
    instance
}

/// The origin-carrying handle of an in-memory tree.
fn memory_tree(data: DataSection) -> TreeHandle {
    Rc::new(RefCell::new(Tree { data, origin: None }))
}

/// Resolve a resource id to its live tree, through the census first and the
/// engine second: the aliasing contract of the reference's cache plus
/// census.
fn census_tree(store_state: &mut Store, resource_id: &str, make_new: bool) -> Option<TreeHandle> {
    if let Some(tree) = store_state.sections.get(resource_id) {
        return Some(tree.clone());
    }
    let loaded = store_state.resource.open_section(resource_id, make_new)?;
    let tree = Rc::new(RefCell::new(Tree {
        data: (*loaded).clone(),
        origin: Some(resource_id.to_owned()),
    }));
    store_state
        .sections
        .insert(resource_id.to_owned(), tree.clone());
    Some(tree)
}

/// The child path of `base` at one tag, as a slash path.
fn child_path(base: &str, tag: &str) -> String {
    if base.is_empty() {
        tag.to_owned()
    } else {
        format!("{base}/{tag}")
    }
}

// ---------------------------------------------------------------------------
// The `guessPythonFromSection` helper of `ResTblStruct::get`
// (`resource_table.cpp:200-247`).
// ---------------------------------------------------------------------------

/// Guess the script value a section carries, with no schema to say: empty
/// reads `None`, digits with fewer than two dots read as numbers, an
/// unambiguous bool reads as `bool`, a `(x, y, z)` text that parses reads
/// as a `Vector3`, anything else stays a string.
///
/// # Safety
///
/// The interpreter must be up and the caller owns the GIL.
unsafe fn guess_python_from_section(section: &DataSection) -> *mut capi::PyObject {
    let text = section.as_string().to_owned();
    if text.is_empty() {
        return capi::py_none();
    }
    // The digit scan: dots counted, any other character breaks.
    let mut dots = 0;
    let mut digits_only = true;
    for character in text.chars() {
        if character == '.' {
            dots += 1;
            continue;
        }
        if !character.is_ascii_digit() {
            digits_only = false;
            break;
        }
    }
    if digits_only && dots < 2 {
        if dots > 0 {
            // SAFETY: the interpreter is up and the caller owns the GIL.
            return float_object(section.as_double(0.0));
        }
        return int_object(section.as_int(0) as i64);
    }
    let as_false = section.as_bool(false);
    if as_false == section.as_bool(true) {
        return boolean_object(as_false);
    }
    if text.starts_with('(') && text.ends_with(')') {
        // The sentinel compare of the reference: a parse that changed
        // anything from the all-sentinel default is a Vector3.
        let sentinel = Vector3::new(f32::MAX, -f32::MAX, f32::MAX);
        let parsed = section.as_vector3(sentinel);
        if parsed != sentinel {
            // SAFETY: the interpreter is up and the caller owns the GIL.
            let vector = unsafe { vector_object([parsed.x, parsed.y, parsed.z, 0.0], 3) };
            return own_raw(vector);
        }
    }
    string_object(&text)
}

// ---------------------------------------------------------------------------
// PyDataSection: the constructor and the protocol slots.
// ---------------------------------------------------------------------------

/// `PyDataSection::pyNew` (`py_data_section.cpp:24-36`): an optional root
/// tag builds a fresh in-memory section; anything else is the refusal.
///
/// # Safety
///
/// A C `tp_new` call: the interpreter holds the GIL.
unsafe extern "C" fn new_data_section(
    _type_object: *mut capi::PyTypeObject,
    args: *mut capi::PyObject,
    _kwds: *mut capi::PyObject,
) -> *mut capi::PyObject {
    let count = unsafe { capi::PyTuple_Size(args) };
    let name = match count {
        0 => String::new(),
        1 => {
            let item = unsafe { capi::PyTuple_GetItem(args, 0) };
            if unsafe { !capi::py_str_check(item) } {
                raise_type_error("PyDataSection() expected an optional string argument");
                return std::ptr::null_mut();
            }
            // SAFETY: the check above guarantees a str object.
            unsafe { capi::to_string(item) }
        }
        _ => {
            raise_type_error("PyDataSection() expected an optional string argument");
            return std::ptr::null_mut();
        }
    };
    let instance = unsafe { section_object(memory_tree(DataSection::new(&name)), String::new()) };
    own_raw(instance)
}

/// `PyDataSection::length` (`py_data_section.cpp:523-526`): the child
/// count, attributes included.
///
/// # Safety
///
/// A C slot call: the interpreter holds the GIL and `self_` is an instance
/// of the type.
unsafe extern "C" fn ds_length(self_: *mut capi::PyObject) -> capi::PySsize {
    let (root, path) = unsafe { section_parts(self_) };
    let tree = root.borrow();
    let node = walk_read(&tree.data, &path);
    node.map_or(0, |section| section.count_children() as capi::PySsize)
}

/// `PyDataSection::subscript` (`py_data_section.cpp:495-521`): the path
/// opens a child, a miss answers `None`, and a non-str key is the
/// `PyUnicode_AsUTF8` failure the reference consumes -- the internal-call
/// `SystemError`.
///
/// # Safety
///
/// A C slot call: the interpreter holds the GIL and `self_` is an instance
/// of the type.
unsafe extern "C" fn ds_subscript(
    self_: *mut capi::PyObject,
    key: *mut capi::PyObject,
) -> *mut capi::PyObject {
    if unsafe { !capi::py_str_check(key) } {
        // The reference's `PyUnicode_AsUTF8` sets the internal-call error
        // and the check below it returns null with that error pending.
        raise_system_error("bad argument to internal function");
        return std::ptr::null_mut();
    }
    // SAFETY: the check above guarantees a str object.
    let path = unsafe { capi::to_string(key) };
    let (root, base) = unsafe { section_parts(self_) };
    let tree = root.borrow();
    let found = walk_read(&tree.data, &base)
        .and_then(|node| walk_read(node, &path))
        .is_some();
    drop(tree);
    if !found {
        return capi::py_none();
    }
    // SAFETY: the interpreter is up and the caller owns the GIL.
    own_raw(unsafe { section_object(root, child_path(&base, &path)) })
}

/// `PyDataSection::pyGetAttribute` (`py_data_section.cpp:362-377`): a
/// single leading underscore (a second underscore stays a dunder) opens
/// the child of that name first, everything else falls to the generic
/// walk.
///
/// # Safety
///
/// A C slot call: the interpreter holds the GIL.
unsafe extern "C" fn ds_getattro(
    self_: *mut capi::PyObject,
    name: *mut capi::PyObject,
) -> *mut capi::PyObject {
    if unsafe { capi::py_str_check(name) } {
        // SAFETY: the check above guarantees a str object.
        let attribute = unsafe { capi::to_string(name) };
        let bytes = attribute.as_bytes();
        if bytes.first() == Some(&b'_') && bytes.get(1) != Some(&b'_') {
            let (root, base) = unsafe { section_parts(self_) };
            let tag = &attribute[1..];
            let tree = root.borrow();
            let found = walk_read(&tree.data, &base)
                .and_then(|node| walk_read(node, tag))
                .is_some();
            drop(tree);
            if found {
                // SAFETY: the interpreter is up and the caller owns the GIL.
                return own_raw(unsafe { section_object(root, child_path(&base, tag)) });
            }
        }
    }
    // SAFETY: the slot contract holds.
    unsafe { capi::PyObject_GenericGetAttr(self_, name) }
}

/// The write ladder of `pySetAttribute` (`py_data_section.cpp:419-483`):
/// str writes text (the unicode branch behind `ScriptString` is dead),
/// a float writes a float -- and the `ScriptFloat` branch shadows
/// `ScriptInt`, so ints write float text -- then the three vector shapes
/// in order, and anything else is the refusal. The child must already
/// exist: an assignment never creates.
///
/// # Safety
///
/// The interpreter holds the GIL and `value` is a live object.
unsafe fn set_attribute_value(
    node: &mut DataSection,
    attribute: &str,
    value: *mut capi::PyObject,
) -> bool {
    if unsafe { capi::py_str_check(value) } {
        // SAFETY: the check above guarantees a str object.
        node.set_string(&unsafe { capi::to_string(value) });
        return true;
    }
    // The `ScriptFloat` branch: floats and ints. No varName surfaces from
    // this ladder -- every failure is cleared until the final refusal.
    let as_float = if unsafe { capi::py_float_check(value) } {
        Some(unsafe { capi::PyFloat_AsDouble(value) })
    } else if unsafe { capi::py_long_check(value) } {
        // SAFETY: the check above guarantees an int object.
        Some(unsafe { capi::PyLong_AsLong(value) } as f64)
    } else {
        None
    };
    if let Some(number) = as_float {
        node.set_float(number as f32);
        return true;
    }
    // The `harmlessSetData` ladder: each probe clears its own failure.
    for dim in 2..=4 {
        // SAFETY: the ladder runs under the GIL.
        if let Some(values) = unsafe { parse_vector_like(value, dim, "") } {
            match dim {
                2 => node.set_vector2(Vector2::new(values[0], values[1])),
                3 => node.set_vector3(Vector3::new(values[0], values[1], values[2])),
                _ => node.set_vector4(Vector4::new(values[0], values[1], values[2], values[3])),
            }
            return true;
        }
        // SAFETY: a failed probe leaves its error pending.
        unsafe { capi::clear_error() };
    }
    raise_type_error(&format!(
        "PyDataSection.{attribute} must be set to a string, float, int, \
         vector2 or vector3 or vector4"
    ));
    false
}

/// `PyDataSection::pySetAttribute` (`py_data_section.cpp:419-483`): any
/// leading underscore writes an EXISTING child (the name minus one
/// underscore), everything else falls to the generic walk.
///
/// # Safety
///
/// A C slot call: the interpreter holds the GIL.
unsafe extern "C" fn ds_setattro(
    self_: *mut capi::PyObject,
    name: *mut capi::PyObject,
    value: *mut capi::PyObject,
) -> c_int {
    if unsafe { capi::py_str_check(name) } {
        // SAFETY: the check above guarantees a str object.
        let attribute = unsafe { capi::to_string(name) };
        if attribute.as_bytes().first() == Some(&b'_') {
            let (root, base) = unsafe { section_parts(self_) };
            let mut tree = root.borrow_mut();
            let written = walk_mut(&mut tree.data, &base)
                .and_then(|node| walk_mut(node, &attribute[1..]))
                .map(|node| unsafe { set_attribute_value(node, &attribute, value) });
            return match written {
                Some(true) => 0,
                Some(false) => -1,
                // A missing child falls through to the generic wall: the
                // reference's miss reaches `PyObjectPlus::pySetAttribute`.
                None => unsafe { capi::PyObject_GenericSetAttr(self_, name, value) },
            };
        }
    }
    // SAFETY: the slot contract holds.
    unsafe { capi::PyObject_GenericSetAttr(self_, name, value) }
}

// ---------------------------------------------------------------------------
// PyDataSection: the attribute family.
// ---------------------------------------------------------------------------

/// A macro over the read-side attribute bodies: resolve the node, answer
/// the conversion -- or the empty-section answer when the wrapper was
/// orphaned (the registered read-defaults delta).
macro_rules! with_section_read {
    ($self_:ident, |$section:ident| $body:expr) => {{
        let (root, path) = unsafe { section_parts($self_) };
        let tree = root.borrow();
        match walk_read(&tree.data, &path) {
            Some($section) => $body,
            None => {
                drop(tree);
                let empty = DataSection::new("");
                let $section = &empty;
                $body
            }
        }
    }};
}

/// A macro over the write-side attribute bodies: resolve the node for
/// mutation or drop the write (the registered orphan delta).
macro_rules! with_section_write {
    ($self_:ident, |$section:ident| $body:expr) => {{
        let (root, path) = unsafe { section_parts($self_) };
        let mut tree = root.borrow_mut();
        if let Some($section) = walk_mut(&mut tree.data, &path) {
            $body
        }
    }};
}

/// The plain bool coercion behind the attribute setters: an `int` tested
/// against zero, or a `"true"`/`"false"` string in any capitalisation
/// (`script.cpp:1544-1568`). `None` on refusal, no error of its own.
///
/// # Safety
///
/// The interpreter holds the GIL and `value` is a live object.
unsafe fn bool_arg_value(value: *mut capi::PyObject) -> Option<bool> {
    if unsafe { capi::py_long_check(value) } {
        // SAFETY: the check above guarantees an int object.
        let value = unsafe { capi::PyLong_AsLong(value) };
        return Some(value != 0);
    }
    if unsafe { capi::py_str_check(value) } {
        // SAFETY: the check above guarantees a str object.
        let text = unsafe { capi::to_string(value) };
        if text.eq_ignore_ascii_case("true") {
            return Some(true);
        }
        if text.eq_ignore_ascii_case("false") {
            return Some(false);
        }
    }
    None
}

/// `isAttribute` getter (`py_data_section.cpp:2024-2028`).
///
/// # Safety
///
/// A C getter call: the interpreter holds the GIL.
unsafe extern "C" fn get_is_attribute(
    self_: *mut capi::PyObject,
    _closure: *mut c_void,
) -> *mut capi::PyObject {
    let answer = with_section_read!(self_, |section| section.is_attribute());
    boolean_object(answer)
}

/// `isAttribute` setter (`py_data_section.cpp:2032-2057`): the bool parse
/// runs first -- the wrong-type `TypeError` wins over the children wall --
/// then a section with children refuses. The reference's final
/// "Failed to set isAttribute" wall is dead code behind its own children
/// check and stays out.
///
/// # Safety
///
/// A C setter call: the interpreter holds the GIL.
unsafe extern "C" fn set_is_attribute(
    self_: *mut capi::PyObject,
    value: *mut capi::PyObject,
    _closure: *mut c_void,
) -> c_int {
    let parsed = unsafe { bool_arg_value(value) };
    let Some(flag) = parsed else {
        raise_type_error("isAttribute must be set to a bool");
        return -1;
    };
    let (root, path) = unsafe { section_parts(self_) };
    let mut tree = root.borrow_mut();
    match walk_mut(&mut tree.data, &path) {
        Some(section) => {
            if section.count_children() != 0 {
                drop(tree);
                raise_attribute_error(
                    "isAttribute can only be used on XML DataSections which have no children",
                );
                return -1;
            }
            section.set_is_attribute(flag);
            0
        }
        // An orphaned wrapper drops the write (the registered delta).
        None => 0,
    }
}

/// The typed attribute setters for the text-valued faces share one shape:
/// the str coercion with the attribute name in the sentence, then write.
macro_rules! text_setter {
    ($name:ident, $sentence:expr, $apply:expr) => {
        /// # Safety
        ///
        /// A C setter call: the interpreter holds the GIL.
        unsafe extern "C" fn $name(
            self_: *mut capi::PyObject,
            value: *mut capi::PyObject,
            _closure: *mut c_void,
        ) -> c_int {
            if unsafe { !capi::py_str_check(value) } {
                raise_type_error($sentence);
                return -1;
            }
            // SAFETY: the check above guarantees a str object.
            let text = unsafe { capi::to_string(value) };
            with_section_write!(self_, |section| {
                let apply: fn(&mut DataSection, &str) = $apply;
                apply(section, &text);
            });
            0
        }
    };
}

text_setter!(
    set_as_string,
    "asString must be set to a string.",
    |section: &mut DataSection, text: &str| section.set_string(text)
);
text_setter!(
    set_as_wide_string,
    "asWideString must be set to a wide string.",
    // The codec is a plain UTF-8 round trip (`xml_section.cpp:2565-2587`).
    |section: &mut DataSection, text: &str| section.set_string(text)
);
text_setter!(
    set_as_blob,
    "asBlob must be set to a string.",
    // `setBlob` stores the bytes base64 encoded
    // (`xml_section.cpp:1268-1274`).
    |section: &mut DataSection, text: &str| section.set_string(&base64::encode(text.as_bytes()))
);

/// The plain int coercion behind the attribute setters: an `int` that fits
/// a C `int`, with the bare sentence raised on refusal.
///
/// # Safety
///
/// The interpreter holds the GIL and `value` is a live object.
unsafe fn coerce_plain_int(value: *mut capi::PyObject) -> Option<i32> {
    if unsafe { capi::py_long_check(value) } {
        // SAFETY: the check above guarantees an int object.
        let value = unsafe { capi::PyLong_AsLong(value) };
        let overflow = unsafe { capi::PyErr_Occurred() }.is_null();
        if overflow {
            let narrowed = value as i32;
            if value as i64 == narrowed as i64 {
                return Some(narrowed);
            }
        } else {
            // SAFETY: the conversion raised an overflow.
            unsafe { capi::clear_error() };
        }
    }
    raise_type_error("must be set to an int");
    None
}

/// The plain int64 coercion behind the attribute setters.
///
/// # Safety
///
/// The interpreter holds the GIL and `value` is a live object.
unsafe fn coerce_plain_int64(value: *mut capi::PyObject) -> Option<i64> {
    if unsafe { capi::py_long_check(value) } {
        // SAFETY: the check above guarantees an int object.
        let value = unsafe { capi::PyLong_AsLong(value) };
        let overflow = unsafe { capi::PyErr_Occurred() }.is_null();
        if overflow {
            return Some(value);
        }
        // SAFETY: the conversion raised an overflow.
        unsafe { capi::clear_error() };
    }
    raise_type_error("must be set to a long");
    None
}

/// The plain float coercion behind the attribute setters: a `float` or an
/// `int` (`script.cpp:1805-1842`).
///
/// # Safety
///
/// The interpreter holds the GIL and `value` is a live object.
unsafe fn coerce_plain_float(value: *mut capi::PyObject) -> Option<f64> {
    if unsafe { capi::py_float_check(value) } {
        // SAFETY: the check above guarantees a float object.
        return Some(unsafe { capi::PyFloat_AsDouble(value) });
    }
    if unsafe { capi::py_long_check(value) } {
        // SAFETY: the check above guarantees an int object.
        let value = unsafe { capi::PyLong_AsLong(value) };
        return Some(value as f64);
    }
    raise_type_error("must be set to a float");
    None
}

/// The plain bool coercion behind the attribute setters.
///
/// # Safety
///
/// The interpreter holds the GIL and `value` is a live object.
unsafe fn coerce_plain_bool(value: *mut capi::PyObject) -> Option<bool> {
    if let Some(flag) = unsafe { bool_arg_value(value) } {
        return Some(flag);
    }
    raise_type_error("must be set to a bool");
    None
}

/// The numeric attribute setters share one shape: coerce with the typed
/// sentence, then write.
macro_rules! number_setter {
    ($name:ident, $coerce:ident, $sentence:expr, $apply:expr) => {
        /// # Safety
        ///
        /// A C setter call: the interpreter holds the GIL.
        unsafe extern "C" fn $name(
            self_: *mut capi::PyObject,
            value: *mut capi::PyObject,
            _closure: *mut c_void,
        ) -> c_int {
            // SAFETY: the coercion runs under the GIL.
            let parsed = unsafe { $coerce(value) };
            let Some(number) = parsed else {
                raise_type_error($sentence);
                return -1;
            };
            with_section_write!(self_, |section| {
                let apply: fn(&mut DataSection, _) = $apply;
                apply(section, number);
            });
            0
        }
    };
}

number_setter!(
    set_as_int,
    coerce_plain_int,
    "asInt must be set to an int",
    |section: &mut DataSection, number: i32| section.set_int(number)
);
number_setter!(
    set_as_int64,
    coerce_plain_int64,
    "asInt64 must be set to a long",
    |section: &mut DataSection, number: i64| section.set_int64(number)
);
number_setter!(
    set_as_float,
    coerce_plain_float,
    "asFloat must be set to a float",
    |section: &mut DataSection, number: f64| section.set_float(number as f32)
);
number_setter!(
    set_as_bool,
    coerce_plain_bool,
    "asBool must be set to a bool",
    // The reference's write is its own no-op: the accessor macro calls
    // `asBool(newVal)`, which binds the GETTER (a default-argument
    // overload) and discards the answer. An assignment parses and changes
    // nothing (`datasection.hpp:417`).
    |_section: &mut DataSection, _flag: bool| {}
);

/// `asString` getter.
///
/// # Safety
///
/// A C getter call: the interpreter holds the GIL.
unsafe extern "C" fn get_as_string(
    self_: *mut capi::PyObject,
    _closure: *mut c_void,
) -> *mut capi::PyObject {
    with_section_read!(self_, |section| string_object(section.as_string()))
}

/// `asWideString` getter: the codec is a plain UTF-8 round trip
/// (`xml_section.cpp:2565-2587`).
///
/// # Safety
///
/// A C getter call: the interpreter holds the GIL.
unsafe extern "C" fn get_as_wide_string(
    self_: *mut capi::PyObject,
    _closure: *mut c_void,
) -> *mut capi::PyObject {
    with_section_read!(self_, |section| string_object(section.as_string()))
}

/// `asInt` getter.
///
/// # Safety
///
/// A C getter call: the interpreter holds the GIL.
unsafe extern "C" fn get_as_int(
    self_: *mut capi::PyObject,
    _closure: *mut c_void,
) -> *mut capi::PyObject {
    with_section_read!(self_, |section| int_object(section.as_int(0) as i64))
}

/// `asInt64` getter.
///
/// # Safety
///
/// A C getter call: the interpreter holds the GIL.
unsafe extern "C" fn get_as_int64(
    self_: *mut capi::PyObject,
    _closure: *mut c_void,
) -> *mut capi::PyObject {
    with_section_read!(self_, |section| int_object(section.as_int64(0)))
}

/// `asFloat` getter.
///
/// # Safety
///
/// A C getter call: the interpreter holds the GIL.
unsafe extern "C" fn get_as_float(
    self_: *mut capi::PyObject,
    _closure: *mut c_void,
) -> *mut capi::PyObject {
    with_section_read!(self_, |section| float_object(section.as_float(0.0) as f64))
}

/// `asBool` getter: the `asBool(false)` of the reference.
///
/// # Safety
///
/// A C getter call: the interpreter holds the GIL.
unsafe extern "C" fn get_as_bool(
    self_: *mut capi::PyObject,
    _closure: *mut c_void,
) -> *mut capi::PyObject {
    let answer = with_section_read!(self_, |section| section.as_bool(false));
    boolean_object(answer)
}

/// One vector attribute getter over `dim`.
///
/// # Safety
///
/// A C getter call: the interpreter holds the GIL.
unsafe fn get_as_vector(self_: *mut capi::PyObject, dim: usize) -> *mut capi::PyObject {
    let values = with_section_read!(self_, |section| match dim {
        2 => {
            let v = section.as_vector2(Vector2::ZERO);
            [v.x, v.y, 0.0, 0.0]
        }
        3 => {
            let v = section.as_vector3(Vector3::ZERO);
            [v.x, v.y, v.z, 0.0]
        }
        _ => {
            let v = section.as_vector4(Vector4::ZERO);
            [v.x, v.y, v.z, v.w]
        }
    });
    // SAFETY: the interpreter is up and the caller owns the GIL.
    own_raw(unsafe { vector_object(values, dim) })
}

/// One vector attribute setter over `dim`: the parse carries the attribute
/// name in the sentence.
///
/// # Safety
///
/// A C setter call: the interpreter holds the GIL.
unsafe fn set_as_vector(
    self_: *mut capi::PyObject,
    value: *mut capi::PyObject,
    attribute: &str,
    dim: usize,
) -> c_int {
    // SAFETY: the setter runs under the GIL.
    let Some(values) = (unsafe { parse_vector_like(value, dim, attribute) }) else {
        // The parse already raised `"<attr> must be set to a VectorN or
        // tuple of N floats"`.
        return -1;
    };
    with_section_write!(self_, |section| match dim {
        2 => section.set_vector2(Vector2::new(values[0], values[1])),
        3 => section.set_vector3(Vector3::new(values[0], values[1], values[2])),
        _ => section.set_vector4(Vector4::new(values[0], values[1], values[2], values[3])),
    });
    0
}

/// `asVector2` getter.
///
/// # Safety
///
/// A C getter call: the interpreter holds the GIL.
unsafe extern "C" fn get_as_vector2(
    self_: *mut capi::PyObject,
    _closure: *mut c_void,
) -> *mut capi::PyObject {
    unsafe { get_as_vector(self_, 2) }
}

/// `asVector2` setter.
///
/// # Safety
///
/// A C setter call: the interpreter holds the GIL.
unsafe extern "C" fn set_as_vector2(
    self_: *mut capi::PyObject,
    value: *mut capi::PyObject,
    _closure: *mut c_void,
) -> c_int {
    unsafe { set_as_vector(self_, value, "asVector2", 2) }
}

/// `asVector3` getter.
///
/// # Safety
///
/// A C getter call: the interpreter holds the GIL.
unsafe extern "C" fn get_as_vector3(
    self_: *mut capi::PyObject,
    _closure: *mut c_void,
) -> *mut capi::PyObject {
    unsafe { get_as_vector(self_, 3) }
}

/// `asVector3` setter.
///
/// # Safety
///
/// A C setter call: the interpreter holds the GIL.
unsafe extern "C" fn set_as_vector3(
    self_: *mut capi::PyObject,
    value: *mut capi::PyObject,
    _closure: *mut c_void,
) -> c_int {
    unsafe { set_as_vector(self_, value, "asVector3", 3) }
}

/// `asVector4` getter.
///
/// # Safety
///
/// A C getter call: the interpreter holds the GIL.
unsafe extern "C" fn get_as_vector4(
    self_: *mut capi::PyObject,
    _closure: *mut c_void,
) -> *mut capi::PyObject {
    unsafe { get_as_vector(self_, 4) }
}

/// `asVector4` setter.
///
/// # Safety
///
/// A C setter call: the interpreter holds the GIL.
unsafe extern "C" fn set_as_vector4(
    self_: *mut capi::PyObject,
    value: *mut capi::PyObject,
    _closure: *mut c_void,
) -> c_int {
    unsafe { set_as_vector(self_, value, "asVector4", 4) }
}

/// `XMLSection::asMatrix34` (`xml_section.cpp:915-935`): the four rows
/// read as three-float children named `rowN` -- a missing row falls back to
/// the default's -- then column 3 is forced to `0,0,0,1`.
fn matrix_from_section(section: &DataSection, default: MatrixValue) -> MatrixValue {
    let mut value = default;
    for (row, cells) in value.m.iter_mut().enumerate() {
        let fallback = Vector3::new(cells[0], cells[1], cells[2]);
        let parsed = section.read_vector3(&format!("row{row}"), fallback);
        cells[0] = parsed.x;
        cells[1] = parsed.y;
        cells[2] = parsed.z;
    }
    value.m[0][3] = 0.0;
    value.m[1][3] = 0.0;
    value.m[2][3] = 0.0;
    value.m[3][3] = 1.0;
    value
}

/// `asMatrix` getter: the zero default through `asMatrix34`.
///
/// # Safety
///
/// A C getter call: the interpreter holds the GIL.
unsafe extern "C" fn get_as_matrix(
    self_: *mut capi::PyObject,
    _closure: *mut c_void,
) -> *mut capi::PyObject {
    let value = with_section_read!(self_, |section| matrix_from_section(
        section,
        MatrixValue::zero()
    ));
    // SAFETY: the interpreter is up and the caller owns the GIL.
    own_raw(unsafe { matrix_object(value) })
}

/// `asMatrix` setter: the `MatrixProvider` coercion, then the four rows
/// write as three-float children (`xml_section.cpp:1246-1257`).
///
/// # Safety
///
/// A C setter call: the interpreter holds the GIL.
unsafe extern "C" fn set_as_matrix(
    self_: *mut capi::PyObject,
    value: *mut capi::PyObject,
    _closure: *mut c_void,
) -> c_int {
    // SAFETY: the setter runs under the GIL.
    let Some(matrix) = (unsafe { matrix_of(value) }) else {
        raise_type_error("asMatrix must be a MatrixProvider");
        return -1;
    };
    with_section_write!(self_, |section| {
        for (row, cells) in matrix.m.iter().enumerate() {
            if let Some(child) = section.open_or_make_section(&format!("row{row}")) {
                child.set_vector3(Vector3::new(cells[0], cells[1], cells[2]));
            }
        }
    });
    0
}

/// `asBinary` getter: the serialised form, `XMLSection::asBinary`
/// (`xml_section.cpp:938-949`).
///
/// # Safety
///
/// A C getter call: the interpreter holds the GIL.
unsafe extern "C" fn get_as_binary(
    self_: *mut capi::PyObject,
    _closure: *mut c_void,
) -> *mut capi::PyObject {
    let bytes = with_section_read!(self_, |section| {
        let mut scratch = section.clone();
        scratch.write_to_string().into_bytes()
    });
    // SAFETY: the interpreter is up and the caller owns the GIL.
    unsafe {
        capi::PyBytes_FromStringAndSize(
            bytes.as_ptr().cast::<c_char>(),
            bytes.len() as capi::PySsize,
        )
    }
}

/// `asBinary` setter: the string parse runs, then `setBinary` -- which on
/// the XML base is a silent no-op (`datasection.cpp:1973`).
///
/// # Safety
///
/// A C setter call: the interpreter holds the GIL.
unsafe extern "C" fn set_as_binary(
    _self_: *mut capi::PyObject,
    value: *mut capi::PyObject,
    _closure: *mut c_void,
) -> c_int {
    if unsafe { !capi::py_str_check(value) } {
        raise_type_error("asBinary must be set to a string.");
        return -1;
    }
    0
}

/// The binary answer of the blob reads: the reference builds a unicode of
/// the raw bytes; a blob that is not UTF-8 has no unicode answer, so the
/// port answers bytes there.
///
/// # Safety
///
/// The interpreter is up and the caller owns the GIL.
unsafe fn blob_object(decoded: &[u8]) -> *mut capi::PyObject {
    match std::str::from_utf8(decoded) {
        // SAFETY: the interpreter is up and the caller owns the GIL.
        Ok(text) => string_object(text),
        Err(_) => unsafe {
            capi::PyBytes_FromStringAndSize(
                decoded.as_ptr().cast::<c_char>(),
                decoded.len() as capi::PySsize,
            )
        },
    }
}

/// `asBlob` getter: the value read as base64 and decoded leniently
/// (`xml_section.cpp:958-974`, the stop-at-`=` decoder).
///
/// # Safety
///
/// A C getter call: the interpreter holds the GIL.
unsafe extern "C" fn get_as_blob(
    self_: *mut capi::PyObject,
    _closure: *mut c_void,
) -> *mut capi::PyObject {
    let decoded = with_section_read!(self_, |section| base64::decode_lenient(section.as_string()));
    // SAFETY: the interpreter is up and the caller owns the GIL.
    unsafe { blob_object(&decoded) }
}

/// `name` getter: the read-only `sectionName` accessor
/// (`py_data_section.hpp:110`).
///
/// # Safety
///
/// A C getter call: the interpreter holds the GIL.
unsafe extern "C" fn get_name(
    self_: *mut capi::PyObject,
    _closure: *mut c_void,
) -> *mut capi::PyObject {
    let name = with_section_read!(self_, |section| section.section_name().to_owned());
    string_object(&name)
}

// ---------------------------------------------------------------------------
// PyDataSection: the read methods.
// ---------------------------------------------------------------------------

/// The read surface, one arm per `IMPLEMENT_READ_VALUE` instantiation
/// (`py_data_section.cpp:719-742`): the type name the failure sentence
/// quotes.
#[derive(Clone, Copy, PartialEq)]
enum ReadKind {
    Str,
    WStr,
    Int,
    Int64,
    Float,
    Bool,
    Vec2,
    Vec3,
    Vec4,
    Matrix,
    Blob,
}

impl ReadKind {
    /// The `#TYPE` text of the reference's macro, verbatim.
    fn label(self) -> &'static str {
        match self {
            ReadKind::Str | ReadKind::Blob => "BW::string",
            ReadKind::WStr => "BW::wstring",
            ReadKind::Int => "int",
            ReadKind::Int64 => "int64",
            ReadKind::Float => "float",
            ReadKind::Bool => "bool",
            ReadKind::Vec2 => "Vector2",
            ReadKind::Vec3 => "Vector3",
            ReadKind::Vec4 => "Vector4",
            ReadKind::Matrix => "Matrix",
        }
    }
}

/// The typed default a read call carries: the value the optional argument
/// parsed into, or the type's zero (`TYPE()` of the reference's macro).
#[derive(Clone)]
enum DefaultValue {
    Owned(String),
    Number(i64),
    Real(f64),
    Flag(bool),
    V2(Vector2),
    V3(Vector3),
    V4(Vector4),
    Mx(MatrixValue),
}

/// The type-zero default of a read kind, when the call passed no default.
fn zero_default(kind: ReadKind) -> DefaultValue {
    match kind {
        ReadKind::Str | ReadKind::WStr | ReadKind::Blob => DefaultValue::Owned(String::new()),
        ReadKind::Int | ReadKind::Int64 => DefaultValue::Number(0),
        ReadKind::Float => DefaultValue::Real(0.0),
        ReadKind::Bool => DefaultValue::Flag(false),
        ReadKind::Vec2 => DefaultValue::V2(Vector2::ZERO),
        ReadKind::Vec3 => DefaultValue::V3(Vector3::ZERO),
        ReadKind::Vec4 => DefaultValue::V4(Vector4::ZERO),
        ReadKind::Matrix => DefaultValue::Mx(MatrixValue::zero()),
    }
}

/// The `Script::setData(pDef, defVal)` of the read macro, per type: the
/// typed sentence raised on refusal (which the caller then replaces).
///
/// # Safety
///
/// The interpreter holds the GIL; `object` is a live object or null.
unsafe fn coerce_read_default(object: *mut capi::PyObject, kind: ReadKind) -> Option<DefaultValue> {
    match kind {
        ReadKind::Str | ReadKind::WStr | ReadKind::Blob => {
            if unsafe { capi::py_str_check(object) } {
                // SAFETY: the check above guarantees a str object.
                return Some(DefaultValue::Owned(unsafe { capi::to_string(object) }));
            }
            raise_type_error("must be set to a string.");
            None
        }
        ReadKind::Int => {
            unsafe { coerce_plain_int(object) }.map(|n| DefaultValue::Number(n as i64))
        }
        ReadKind::Int64 => unsafe { coerce_plain_int64(object) }.map(DefaultValue::Number),
        ReadKind::Float => unsafe { coerce_plain_float(object) }.map(DefaultValue::Real),
        ReadKind::Bool => unsafe { coerce_plain_bool(object) }.map(DefaultValue::Flag),
        ReadKind::Vec2 | ReadKind::Vec3 | ReadKind::Vec4 => {
            let dim = match kind {
                ReadKind::Vec2 => 2,
                ReadKind::Vec3 => 3,
                _ => 4,
            };
            // SAFETY: the coercion runs under the GIL.
            unsafe { parse_vector_like(object, dim, "") }.map(|values| match dim {
                2 => DefaultValue::V2(Vector2::new(values[0], values[1])),
                3 => DefaultValue::V3(Vector3::new(values[0], values[1], values[2])),
                _ => DefaultValue::V4(Vector4::new(values[0], values[1], values[2], values[3])),
            })
        }
        ReadKind::Matrix => {
            // SAFETY: the coercion runs under the GIL.
            unsafe { matrix_of(object) }.map(DefaultValue::Mx)
        }
    }
}

/// `Script::getData` of the found-section conversion, per type: the read
/// goes through the engine's `as*` against the default
/// (`datasection.hpp:417`).
fn section_answer(
    section: &DataSection,
    kind: ReadKind,
    default: DefaultValue,
) -> *mut capi::PyObject {
    match kind {
        ReadKind::Str | ReadKind::WStr => string_object(section.as_string()),
        ReadKind::Blob => {
            let decoded = base64::decode_lenient(section.as_string());
            // SAFETY: the caller owns the GIL.
            unsafe { blob_object(&decoded) }
        }
        ReadKind::Int => {
            let DefaultValue::Number(number) = default else {
                unreachable!("an int read carries a number default")
            };
            int_object(section.as_int(number as i32) as i64)
        }
        ReadKind::Int64 => {
            let DefaultValue::Number(number) = default else {
                unreachable!("an int64 read carries a number default")
            };
            int_object(section.as_int64(number))
        }
        ReadKind::Float => {
            let DefaultValue::Real(real) = default else {
                unreachable!("a float read carries a real default")
            };
            float_object(section.as_float(real as f32) as f64)
        }
        ReadKind::Bool => {
            let DefaultValue::Flag(flag) = default else {
                unreachable!("a bool read carries a flag default")
            };
            boolean_object(section.as_bool(flag))
        }
        ReadKind::Vec2 => {
            let DefaultValue::V2(v2) = default else {
                unreachable!("a Vector2 read carries a v2 default")
            };
            let v = section.as_vector2(v2);
            // SAFETY: the caller owns the GIL.
            unsafe { own_raw(vector_object([v.x, v.y, 0.0, 0.0], 2)) }
        }
        ReadKind::Vec3 => {
            let DefaultValue::V3(v3) = default else {
                unreachable!("a Vector3 read carries a v3 default")
            };
            let v = section.as_vector3(v3);
            // SAFETY: the caller owns the GIL.
            unsafe { own_raw(vector_object([v.x, v.y, v.z, 0.0], 3)) }
        }
        ReadKind::Vec4 => {
            let DefaultValue::V4(v4) = default else {
                unreachable!("a Vector4 read carries a v4 default")
            };
            let v = section.as_vector4(v4);
            // SAFETY: the caller owns the GIL.
            unsafe { own_raw(vector_object([v.x, v.y, v.z, v.w], 4)) }
        }
        ReadKind::Matrix => {
            let DefaultValue::Mx(mx) = default else {
                unreachable!("a Matrix read carries a matrix default")
            };
            let value = matrix_from_section(section, mx);
            // SAFETY: the caller owns the GIL.
            unsafe { own_raw(matrix_object(value)) }
        }
    }
}

/// The miss answer: the default unchanged, as the reference's
/// `read<TYPE>(path, default)` answers on a missing path.
fn default_answer(default: DefaultValue) -> *mut capi::PyObject {
    match default {
        DefaultValue::Owned(text) => string_object(&text),
        DefaultValue::Number(number) => int_object(number),
        DefaultValue::Real(real) => float_object(real),
        DefaultValue::Flag(flag) => boolean_object(flag),
        DefaultValue::V2(v2) => {
            // SAFETY: the caller owns the GIL.
            unsafe { own_raw(vector_object([v2.x, v2.y, 0.0, 0.0], 2)) }
        }
        DefaultValue::V3(v3) => {
            // SAFETY: the caller owns the GIL.
            unsafe { own_raw(vector_object([v3.x, v3.y, v3.z, 0.0], 3)) }
        }
        DefaultValue::V4(v4) => {
            // SAFETY: the caller owns the GIL.
            unsafe { own_raw(vector_object([v4.x, v4.y, v4.z, v4.w], 4)) }
        }
        DefaultValue::Mx(mx) => {
            // SAFETY: the caller owns the GIL.
            unsafe { own_raw(matrix_object(mx)) }
        }
    }
}

/// The `IMPLEMENT_READ_VALUE` body: parse `s|O`, coerce the optional
/// default, then read. A wrong-shaped call or a refused default replaces
/// whatever the coercion raised with the generic sentence
/// (`py_data_section.cpp:719-742`).
///
/// # Safety
///
/// The interpreter holds the GIL; `self_` is a section instance.
unsafe fn read_value(
    self_: *mut capi::PyObject,
    args: *mut capi::PyObject,
    kind: ReadKind,
) -> *mut capi::PyObject {
    let count = unsafe { capi::PyTuple_Size(args) };
    if !(1..=2).contains(&count) {
        raise_type_error(&format!(
            "Expected a string and an optional {}",
            kind.label()
        ));
        return std::ptr::null_mut();
    }
    let path_object = unsafe { capi::PyTuple_GetItem(args, 0) };
    if unsafe { !capi::py_str_check(path_object) } {
        raise_type_error(&format!(
            "Expected a string and an optional {}",
            kind.label()
        ));
        return std::ptr::null_mut();
    }
    // SAFETY: the check above guarantees a str object.
    let path = unsafe { capi::to_string(path_object) };
    let default = if count == 2 {
        let given = unsafe { capi::PyTuple_GetItem(args, 1) };
        // SAFETY: the coercion runs under the GIL.
        let coerced = unsafe { coerce_read_default(given, kind) };
        match coerced {
            Some(value) => value,
            None => {
                raise_type_error(&format!(
                    "Expected a string and an optional {}",
                    kind.label()
                ));
                return std::ptr::null_mut();
            }
        }
    } else {
        zero_default(kind)
    };
    let (root, base) = unsafe { section_parts(self_) };
    let tree = root.borrow();
    let answer = walk_read(&tree.data, &base).and_then(|node| walk_read(node, &path));
    match answer {
        Some(section) => section_answer(section, kind, default),
        None => default_answer(default),
    }
}

/// The read faces.
macro_rules! read_face {
    ($name:ident, $kind:expr) => {
        /// # Safety
        ///
        /// A C method call: the interpreter holds the GIL.
        unsafe extern "C" fn $name(
            self_: *mut capi::PyObject,
            args: *mut capi::PyObject,
        ) -> *mut capi::PyObject {
            unsafe { read_value(self_, args, $kind) }
        }
    };
}

read_face!(method_read_string, ReadKind::Str);
read_face!(method_read_wide_string, ReadKind::WStr);
read_face!(method_read_int, ReadKind::Int);
read_face!(method_read_int64, ReadKind::Int64);
read_face!(method_read_float, ReadKind::Float);
read_face!(method_read_vector2, ReadKind::Vec2);
read_face!(method_read_vector3, ReadKind::Vec3);
read_face!(method_read_vector4, ReadKind::Vec4);
read_face!(method_read_matrix, ReadKind::Matrix);
read_face!(method_read_bool, ReadKind::Bool);
read_face!(method_read_blob, ReadKind::Blob);

// ---------------------------------------------------------------------------
// PyDataSection: the read*s family.
// ---------------------------------------------------------------------------

/// The `getStringArg` wall (`py_data_section.cpp:704-716`).
///
/// # Safety
///
/// The interpreter holds the GIL.
unsafe fn one_string_argument(args: *mut capi::PyObject) -> Option<String> {
    if unsafe { capi::PyTuple_Size(args) } == 1 {
        let item = unsafe { capi::PyTuple_GetItem(args, 0) };
        if unsafe { capi::py_str_check(item) } {
            // SAFETY: the check above guarantees a str object.
            return Some(unsafe { capi::to_string(item) });
        }
    }
    raise_type_error("Expected one string argument.");
    None
}

/// The `IMPLEMENT_READ_VALUES` body (`py_data_section.cpp:742-764`): every
/// child carrying the leaf tag converts, into a tuple.
///
/// # Safety
///
/// The interpreter holds the GIL; `self_` is a section instance.
unsafe fn read_values(
    self_: *mut capi::PyObject,
    args: *mut capi::PyObject,
    kind: ReadKind,
) -> *mut capi::PyObject {
    let Some(path) = (unsafe { one_string_argument(args) }) else {
        return std::ptr::null_mut();
    };
    let (root, base) = unsafe { section_parts(self_) };
    let tree = root.borrow();
    let mut answers: Vec<*mut capi::PyObject> = Vec::new();
    if let Some(node) = walk_read(&tree.data, &base)
        && let Some((parent, leaf)) = split_tag_read(node, &path)
    {
        for index in 0..parent.count_children() {
            let Some(child) = parent.open_child(index) else {
                continue;
            };
            if child.section_name() != leaf {
                continue;
            }
            answers.push(match kind {
                ReadKind::Str | ReadKind::WStr => string_object(child.as_string()),
                ReadKind::Int => int_object(child.as_int(0) as i64),
                ReadKind::Float => float_object(child.as_float(0.0) as f64),
                ReadKind::Vec2 => {
                    let v = child.as_vector2(Vector2::ZERO);
                    // SAFETY: the caller owns the GIL.
                    unsafe { own_raw(vector_object([v.x, v.y, 0.0, 0.0], 2)) }
                }
                ReadKind::Vec3 => {
                    let v = child.as_vector3(Vector3::ZERO);
                    // SAFETY: the caller owns the GIL.
                    unsafe { own_raw(vector_object([v.x, v.y, v.z, 0.0], 3)) }
                }
                ReadKind::Vec4 => {
                    let v = child.as_vector4(Vector4::ZERO);
                    // SAFETY: the caller owns the GIL.
                    unsafe { own_raw(vector_object([v.x, v.y, v.z, v.w], 4)) }
                }
                _ => unreachable!("read*s carry string, int, float and vector kinds"),
            });
        }
    }
    let tuple = unsafe { capi::PyTuple_New(answers.len() as capi::PySsize) };
    for (index, answer) in answers.into_iter().enumerate() {
        // SAFETY: the tuple is fresh and the answer is an owned reference;
        // `SetItem` steals it.
        unsafe { capi::PyTuple_SetItem(tuple, index as capi::PySsize, answer) };
    }
    tuple
}

/// The read*s faces.
macro_rules! read_values_face {
    ($name:ident, $kind:expr) => {
        /// # Safety
        ///
        /// A C method call: the interpreter holds the GIL.
        unsafe extern "C" fn $name(
            self_: *mut capi::PyObject,
            args: *mut capi::PyObject,
        ) -> *mut capi::PyObject {
            unsafe { read_values(self_, args, $kind) }
        }
    };
}

read_values_face!(method_read_strings, ReadKind::Str);
read_values_face!(method_read_wide_strings, ReadKind::WStr);
read_values_face!(method_read_ints, ReadKind::Int);
read_values_face!(method_read_floats, ReadKind::Float);
read_values_face!(method_read_vector2s, ReadKind::Vec2);
read_values_face!(method_read_vector3s, ReadKind::Vec3);
read_values_face!(method_read_vector4s, ReadKind::Vec4);

// ---------------------------------------------------------------------------
// PyDataSection: the write methods.
// ---------------------------------------------------------------------------

/// The write surface, one arm per `IMPLEMENT_WRITE_VALUE` instantiation.
#[derive(Clone, Copy, PartialEq)]
enum WriteKind {
    Str,
    WStr,
    Int,
    Int64,
    Float,
    Bool,
    Vec2,
    Vec3,
    Vec4,
    Matrix,
    Blob,
}

impl WriteKind {
    /// The `#TYPE` text of the reference's macro, verbatim.
    fn label(self) -> &'static str {
        match self {
            WriteKind::Str => "BW::string",
            WriteKind::WStr => "BW::wstring",
            WriteKind::Int => "int",
            WriteKind::Int64 => "int64",
            WriteKind::Float => "float",
            WriteKind::Bool => "bool",
            WriteKind::Vec2 => "Vector2",
            WriteKind::Vec3 => "Vector3",
            WriteKind::Vec4 => "Vector4",
            WriteKind::Matrix => "Matrix",
            WriteKind::Blob => "BW::string",
        }
    }
}

/// The value wall of the write macro, purely as a check: the write site
/// re-parses so the wall and the write share one code path per the
/// reference's macro.
///
/// # Safety
///
/// The interpreter holds the GIL; `value` is a live object.
unsafe fn coerce_write(value: *mut capi::PyObject, kind: WriteKind) -> bool {
    match kind {
        WriteKind::Str | WriteKind::WStr | WriteKind::Blob => unsafe { capi::py_str_check(value) },
        WriteKind::Int | WriteKind::Int64 => unsafe { capi::py_long_check(value) },
        WriteKind::Float => {
            (unsafe { capi::py_float_check(value) }) || (unsafe { capi::py_long_check(value) })
        }
        WriteKind::Bool => {
            (unsafe { capi::py_long_check(value) })
                || ((unsafe { capi::py_str_check(value) }) && {
                    // SAFETY: the check above guarantees a str object.
                    let text = unsafe { capi::to_string(value) };
                    text.eq_ignore_ascii_case("true") || text.eq_ignore_ascii_case("false")
                })
        }
        WriteKind::Vec2 | WriteKind::Vec3 | WriteKind::Vec4 => {
            let dim = match kind {
                WriteKind::Vec2 => 2,
                WriteKind::Vec3 => 3,
                _ => 4,
            };
            // SAFETY: the probe runs under the GIL.
            unsafe { parse_vector_like(value, dim, "") }.is_some()
        }
        WriteKind::Matrix => {
            // SAFETY: the probe runs under the GIL.
            unsafe { matrix_of(value) }.is_some()
        }
    }
}

/// The write itself, per type: the value re-parsed at the site.
///
/// # Safety
///
/// The interpreter holds the GIL; `value` is a live object the wall
/// accepted.
unsafe fn apply_write(target: &mut DataSection, value: *mut capi::PyObject, kind: WriteKind) {
    match kind {
        WriteKind::Str | WriteKind::WStr => {
            // SAFETY: the wall above guaranteed a str.
            let text = unsafe { capi::to_string(value) };
            target.set_string(&text);
        }
        WriteKind::Blob => {
            // SAFETY: the wall above guaranteed a str.
            let text = unsafe { capi::to_string(value) };
            target.set_string(&base64::encode(text.as_bytes()));
        }
        WriteKind::Int => {
            // SAFETY: the wall above guaranteed an int.
            let number = (unsafe { capi::PyLong_AsLong(value) }) as i32;
            target.set_int(number);
        }
        WriteKind::Int64 => {
            // SAFETY: the wall above guaranteed an int.
            let number = unsafe { capi::PyLong_AsLong(value) };
            target.set_int64(number);
        }
        WriteKind::Float => {
            // SAFETY: the wall above guaranteed a float or int.
            let number = if unsafe { capi::py_float_check(value) } {
                unsafe { capi::PyFloat_AsDouble(value) }
            } else {
                (unsafe { capi::PyLong_AsLong(value) }) as f64
            };
            target.set_float(number as f32);
        }
        WriteKind::Bool => {
            // SAFETY: the wall above guaranteed an int or a true/false str.
            let flag = if unsafe { capi::py_long_check(value) } {
                (unsafe { capi::PyLong_AsLong(value) }) != 0
            } else {
                unsafe { capi::to_string(value) }.eq_ignore_ascii_case("true")
            };
            target.set_bool(flag);
        }
        WriteKind::Vec2 | WriteKind::Vec3 | WriteKind::Vec4 => {
            let dim = match kind {
                WriteKind::Vec2 => 2,
                WriteKind::Vec3 => 3,
                _ => 4,
            };
            // SAFETY: the wall above guaranteed the shape.
            let values = unsafe { parse_vector_like(value, dim, "").expect("the wall parsed") };
            match dim {
                2 => target.set_vector2(Vector2::new(values[0], values[1])),
                3 => target.set_vector3(Vector3::new(values[0], values[1], values[2])),
                _ => target.set_vector4(Vector4::new(values[0], values[1], values[2], values[3])),
            }
        }
        WriteKind::Matrix => {
            // SAFETY: the wall above guaranteed a provider or matrix.
            let matrix = unsafe { matrix_of(value) }.expect("the wall parsed");
            for (row, cells) in matrix.m.iter().enumerate() {
                if let Some(child) = target.open_or_make_section(&format!("row{row}")) {
                    child.set_vector3(Vector3::new(cells[0], cells[1], cells[2]));
                }
            }
        }
    }
}

/// The `IMPLEMENT_WRITE_VALUE` body (`py_data_section.cpp:766-787`): parse
/// `sO`, wall the value, write through the created path, answer the
/// subscript of the path. The reference's "Write failed." arm is dead in
/// the port -- the path creation cannot fail on a live node -- and stays
/// out; an orphaned wrapper drops the write and answers `None`.
///
/// # Safety
///
/// The interpreter holds the GIL; `self_` is a section instance.
unsafe fn write_value(
    self_: *mut capi::PyObject,
    args: *mut capi::PyObject,
    kind: WriteKind,
) -> *mut capi::PyObject {
    let count = unsafe { capi::PyTuple_Size(args) };
    let path_object = if count == 2 {
        unsafe { capi::PyTuple_GetItem(args, 0) }
    } else {
        std::ptr::null_mut()
    };
    if count != 2 || unsafe { !capi::py_str_check(path_object) } {
        raise_type_error(&format!("Expected string and {}.", kind.label()));
        return std::ptr::null_mut();
    }
    // SAFETY: the check above guarantees a str object.
    let path = unsafe { capi::to_string(path_object) };
    let value = unsafe { capi::PyTuple_GetItem(args, 1) };
    if !unsafe { coerce_write(value, kind) } {
        raise_type_error(&format!("Expected string and {}.", kind.label()));
        return std::ptr::null_mut();
    }
    let (root, base) = unsafe { section_parts(self_) };
    let mut tree = root.borrow_mut();
    if let Some(node) = walk_mut(&mut tree.data, &base)
        && let Some(target) = node.open_or_make_section(&path)
    {
        // SAFETY: the wall above accepted the value.
        unsafe { apply_write(target, value, kind) };
    }
    drop(tree);
    // The answer is the subscript of the path: the freshly written child,
    // or `None` when the wrapper was orphaned.
    // SAFETY: the slot contract holds.
    unsafe { ds_subscript(self_, path_object) }
}

/// The write faces.
macro_rules! write_face {
    ($name:ident, $kind:expr) => {
        /// # Safety
        ///
        /// A C method call: the interpreter holds the GIL.
        unsafe extern "C" fn $name(
            self_: *mut capi::PyObject,
            args: *mut capi::PyObject,
        ) -> *mut capi::PyObject {
            unsafe { write_value(self_, args, $kind) }
        }
    };
}

write_face!(method_write_string, WriteKind::Str);
write_face!(method_write_wide_string, WriteKind::WStr);
write_face!(method_write_int, WriteKind::Int);
write_face!(method_write_int64, WriteKind::Int64);
write_face!(method_write_float, WriteKind::Float);
write_face!(method_write_vector2, WriteKind::Vec2);
write_face!(method_write_vector3, WriteKind::Vec3);
write_face!(method_write_vector4, WriteKind::Vec4);
write_face!(method_write_matrix, WriteKind::Matrix);
write_face!(method_write_bool, WriteKind::Bool);
write_face!(method_write_blob, WriteKind::Blob);

// ---------------------------------------------------------------------------
// PyDataSection: the write*s family.
// ---------------------------------------------------------------------------

/// The `IMPLEMENT_WRITE_VALUES` body (`py_data_section.cpp:792-823`): wall
/// the whole sequence first -- the reference builds the full vector before
/// the engine call -- then append one child per item under the leaf tag.
/// The reference's per-item `PyErr_Format(args, ...)` would crash a 3.x
/// interpreter; the port raises the same sentence as a proper `TypeError`.
///
/// # Safety
///
/// The interpreter holds the GIL; `self_` is a section instance.
unsafe fn write_values(
    self_: *mut capi::PyObject,
    args: *mut capi::PyObject,
    kind: WriteKind,
) -> *mut capi::PyObject {
    let count = unsafe { capi::PyTuple_Size(args) };
    let path_object = if count == 2 {
        unsafe { capi::PyTuple_GetItem(args, 0) }
    } else {
        std::ptr::null_mut()
    };
    let sequence = if count == 2 {
        unsafe { capi::PyTuple_GetItem(args, 1) }
    } else {
        std::ptr::null_mut()
    };
    if count != 2
        || unsafe { !capi::py_str_check(path_object) }
        || unsafe { capi::PySequence_Check(sequence) } == 0
    {
        raise_type_error("Expected string and a sequence.");
        return std::ptr::null_mut();
    }
    // SAFETY: the check above guarantees a str object.
    let path = unsafe { capi::to_string(path_object) };
    let size = unsafe { capi::PySequence_Size(sequence) };
    if size < 0 {
        // A sequence whose size fails leaves its own error pending.
        return std::ptr::null_mut();
    }
    let item_kind = match kind {
        WriteKind::Str | WriteKind::WStr | WriteKind::Blob => "BW::string",
        other => other.label(),
    };
    // The items stay alive for the write below: the wall and the write
    // re-parse each one, and the reference sets `values[i]` per item
    // (`py_data_section.cpp:806-820`) -- the sequence itself is never the
    // value.
    let items: Vec<*mut capi::PyObject> = (0..size)
        .map(|index| unsafe { capi::PySequence_GetItem(sequence, index) })
        .collect();
    for (index, item) in items.iter().enumerate() {
        if !unsafe { coerce_write(*item, kind) } {
            // SAFETY: every reference above is this function's to drop.
            for acquired in &items {
                unsafe { capi::dec_ref(*acquired) };
            }
            raise_type_error(&format!("Section {index} is not of of type {item_kind}."));
            return std::ptr::null_mut();
        }
    }
    let (root, base) = unsafe { section_parts(self_) };
    let mut tree = root.borrow_mut();
    if let Some(node) = walk_mut(&mut tree.data, &base)
        && let Some((parent, leaf)) = split_tag_path(node, &path, true)
    {
        for item in &items {
            let child = parent.add_child(&leaf);
            // SAFETY: the wall above accepted every item; the value
            // carries the shape and each child write re-parses it.
            unsafe { apply_write(child, *item, kind) };
        }
    }
    drop(tree);
    // SAFETY: the write is done and the loop owns every reference.
    for item in &items {
        unsafe { capi::dec_ref(*item) };
    }
    capi::py_none()
}

/// The write*s faces.
macro_rules! write_values_face {
    ($name:ident, $kind:expr) => {
        /// # Safety
        ///
        /// A C method call: the interpreter holds the GIL.
        unsafe extern "C" fn $name(
            self_: *mut capi::PyObject,
            args: *mut capi::PyObject,
        ) -> *mut capi::PyObject {
            unsafe { write_values(self_, args, $kind) }
        }
    };
}

write_values_face!(method_write_strings, WriteKind::Str);
write_values_face!(method_write_wide_strings, WriteKind::WStr);
write_values_face!(method_write_ints, WriteKind::Int);
write_values_face!(method_write_floats, WriteKind::Float);
write_values_face!(method_write_vector2s, WriteKind::Vec2);
write_values_face!(method_write_vector3s, WriteKind::Vec3);
write_values_face!(method_write_vector4s, WriteKind::Vec4);

// ---------------------------------------------------------------------------
// PyDataSection: the mapping-style membership and listing faces.
// ---------------------------------------------------------------------------

/// `py_has_key` (`py_data_section.cpp:546-567`): the "s" parse refusal is
/// the fixed sentence; the answer is a bool.
///
/// # Safety
///
/// A C method call: the interpreter holds the GIL.
unsafe extern "C" fn method_has_key(
    self_: *mut capi::PyObject,
    args: *mut capi::PyObject,
) -> *mut capi::PyObject {
    let key = unsafe { capi::PyTuple_GetItem(args, 0) };
    if unsafe { capi::PyTuple_Size(args) } != 1 || unsafe { !capi::py_str_check(key) } {
        raise_type_error("Expected a string argument.");
        return std::ptr::null_mut();
    }
    // SAFETY: the check above guarantees a str object.
    let path = unsafe { capi::to_string(key) };
    let answer = with_section_read!(self_, |section| walk_read(section, &path).is_some());
    boolean_object(answer)
}

/// The leaf tags of a node, in document order.
fn child_tags(section: &DataSection) -> Vec<String> {
    (0..section.count_children())
        .filter_map(|index| {
            section
                .open_child(index)
                .map(|child| child.section_name().to_owned())
        })
        .collect()
}

/// `py_keys` (`py_data_section.cpp:581-598`): the child tag names.
///
/// # Safety
///
/// A C method call: the interpreter holds the GIL.
unsafe extern "C" fn method_keys(
    self_: *mut capi::PyObject,
    _args: *mut capi::PyObject,
) -> *mut capi::PyObject {
    let names = with_section_read!(self_, |section| child_tags(section));
    let list = unsafe { capi::PyList_New(names.len() as capi::PySsize) };
    for (index, name) in names.iter().enumerate() {
        // SAFETY: a fresh list steals the fresh objects.
        unsafe { capi::PyList_SetItem(list, index as capi::PySsize, string_object(name)) };
    }
    list
}

/// The shared body of `values` and `items`: a wrapper per child, addressed
/// by tag path -- duplicate tags answer wrappers of the FIRST child of
/// that name, the registered collapse.
///
/// # Safety
///
/// The interpreter holds the GIL and `self_` is a section instance.
unsafe fn children_of(self_: *mut capi::PyObject) -> Vec<(String, TreeHandle, String)> {
    let (root, base) = unsafe { section_parts(self_) };
    let tags = {
        let tree = root.borrow();
        walk_read(&tree.data, &base).map_or_else(Vec::new, child_tags)
    };
    tags.into_iter()
        .map(|tag| {
            let path = child_path(&base, &tag);
            (tag, root.clone(), path)
        })
        .collect()
}

/// `py_values` (`py_data_section.cpp:612-629`).
///
/// # Safety
///
/// A C method call: the interpreter holds the GIL.
unsafe extern "C" fn method_values(
    self_: *mut capi::PyObject,
    _args: *mut capi::PyObject,
) -> *mut capi::PyObject {
    let children = unsafe { children_of(self_) };
    let list = unsafe { capi::PyList_New(children.len() as capi::PySsize) };
    for (index, (_, root, path)) in children.into_iter().enumerate() {
        // SAFETY: the interpreter is up and the caller owns the GIL.
        let wrapper = unsafe { section_object(root, path) };
        // SAFETY: a fresh list steals the fresh wrapper.
        unsafe { capi::PyList_SetItem(list, index as capi::PySsize, own_raw(wrapper)) };
    }
    list
}

/// `py_items` (`py_data_section.cpp:644-672`).
///
/// # Safety
///
/// A C method call: the interpreter holds the GIL.
unsafe extern "C" fn method_items(
    self_: *mut capi::PyObject,
    _args: *mut capi::PyObject,
) -> *mut capi::PyObject {
    let children = unsafe { children_of(self_) };
    let list = unsafe { capi::PyList_New(children.len() as capi::PySsize) };
    for (index, (tag, root, path)) in children.into_iter().enumerate() {
        let tuple = unsafe { capi::PyTuple_New(2) };
        // SAFETY: a fresh tuple steals the fresh objects.
        unsafe { capi::PyTuple_SetItem(tuple, 0, string_object(&tag)) };
        // SAFETY: the interpreter is up and the caller owns the GIL.
        let wrapper = unsafe { section_object(root, path) };
        unsafe { capi::PyTuple_SetItem(tuple, 1, own_raw(wrapper)) };
        // SAFETY: a fresh list steals the fresh tuple.
        unsafe { capi::PyList_SetItem(list, index as capi::PySsize, tuple) };
    }
    list
}

// ---------------------------------------------------------------------------
// PyDataSection: name cleaning, creation and deletion.
// ---------------------------------------------------------------------------

/// `py_cleanName` (`py_data_section.cpp:1625-1631`): sanitise the tag in
/// place and answer whether it changed. Arguments are ignored, the way the
/// reference signature takes none.
///
/// # Safety
///
/// A C method call: the interpreter holds the GIL.
unsafe extern "C" fn method_clean_name(
    self_: *mut capi::PyObject,
    _args: *mut capi::PyObject,
) -> *mut capi::PyObject {
    let (root, path) = unsafe { section_parts(self_) };
    let mut tree = root.borrow_mut();
    let changed =
        walk_mut(&mut tree.data, &path).is_some_and(|section| section.sanitise_section_name());
    drop(tree);
    boolean_object(changed)
}

/// `py_isNameClean` (`py_data_section.cpp:1642-1648`): the gated tag
/// check, always true with the static off.
///
/// # Safety
///
/// A C method call: the interpreter holds the GIL.
unsafe extern "C" fn method_is_name_clean(
    self_: *mut capi::PyObject,
    _args: *mut capi::PyObject,
) -> *mut capi::PyObject {
    let answer = with_section_read!(self_, |section| section.is_valid_section_name());
    boolean_object(answer)
}

/// `BWResource::getFilePath`/`getFilename` with the `py_createSection`
/// handling (`py_data_section.cpp:1709-1723`): the directory part loses a
/// trailing separator, and a bare name is "." which clears to empty --
/// then the creation happens under the section itself.
fn split_file_path(path: &str) -> (String, String) {
    let mut base = resources::bwresource::get_file_path(path);
    if base == "." {
        base.clear();
    } else if base.ends_with('/') {
        base.pop();
    }
    (base, resources::bwresource::get_filename(path).to_owned())
}

/// `py_createSection` (`py_data_section.cpp:1700-1743`): always append a
/// new child, even over an existing name; the base directories are created
/// on the way.
///
/// # Safety
///
/// A C method call: the interpreter holds the GIL.
unsafe extern "C" fn method_create_section(
    self_: *mut capi::PyObject,
    args: *mut capi::PyObject,
) -> *mut capi::PyObject {
    let Some(path) = (unsafe { one_string_argument(args) }) else {
        return std::ptr::null_mut();
    };
    let (section_base, section_name) = split_file_path(&path);
    let (root, base) = unsafe { section_parts(self_) };
    let mut tree = root.borrow_mut();
    let Some(node) = walk_mut(&mut tree.data, &base) else {
        // An orphaned wrapper cannot create: the registered delta answers
        // None where the reference's detached node still would.
        return capi::py_none();
    };
    let base_path = child_path(&base, &section_base);
    let target = if section_base.is_empty() {
        Some(node)
    } else {
        node.open_or_make_section(&section_base)
    };
    let Some(target) = target else {
        return capi::py_none();
    };
    target.add_child(&section_name);
    drop(tree);
    // The wrapper answers the NEW leaf, not the parent it was added under:
    // the reference wraps `newSection`'s result
    // (`py_data_section.cpp:1700-1740`), and a bare tag's dirname is "" so
    // the parent path alone would name the root the leaf was added to.
    let leaf_path = child_path(&base_path, &section_name);
    // SAFETY: the interpreter is up and the caller owns the GIL.
    own_raw(unsafe { section_object(root, leaf_path) })
}

/// `py_createSectionFromString` (`py_data_section.cpp:1785-1814`): parse
/// the text, copy the parse under a new child of the section name -- and
/// answer the DETACHED parse, not the copy that landed, the reference's
/// kept defect.
///
/// # Safety
///
/// A C method call: the interpreter holds the GIL.
unsafe extern "C" fn method_create_section_from_string(
    self_: *mut capi::PyObject,
    args: *mut capi::PyObject,
) -> *mut capi::PyObject {
    let Some(text) = (unsafe { one_string_argument(args) }) else {
        return std::ptr::null_mut();
    };
    let parsed = match parse_xml("", text.as_bytes()) {
        Ok(parsed) => parsed,
        Err(_) => return capi::py_none(),
    };
    let name = parsed.section_name().to_owned();
    let (root, base) = unsafe { section_parts(self_) };
    let mut tree = root.borrow_mut();
    if let Some(node) = walk_mut(&mut tree.data, &base) {
        node.add_child(&name).copy_from(&parsed);
    }
    drop(tree);
    // The detached parse answers, in a tree of its own.
    // SAFETY: the interpreter is up and the caller owns the GIL.
    own_raw(unsafe { section_object(memory_tree(parsed), String::new()) })
}

/// `py_deleteSection` (`py_data_section.cpp:1834-1880`): a string path
/// deletes through the engine, a wrapper deletes the first child of its
/// name and always answers true, and the wrong type is the TAB-sentence
/// wall.
///
/// # Safety
///
/// A C method call: the interpreter holds the GIL.
unsafe extern "C" fn method_delete_section(
    self_: *mut capi::PyObject,
    args: *mut capi::PyObject,
) -> *mut capi::PyObject {
    if unsafe { capi::PyTuple_Size(args) } != 1 {
        raise_type_error("py_deleteSection: Expected a DataSection or String argument.");
        return std::ptr::null_mut();
    }
    let item = unsafe { capi::PyTuple_GetItem(args, 0) };
    if unsafe { capi::py_str_check(item) } {
        // SAFETY: the check above guarantees a str object.
        let path = unsafe { capi::to_string(item) };
        let (root, base) = unsafe { section_parts(self_) };
        let mut tree = root.borrow_mut();
        let deleted =
            walk_mut(&mut tree.data, &base).is_some_and(|node| node.delete_section(&path));
        drop(tree);
        return boolean_object(deleted);
    }
    if unsafe { check_type(item, DATA_SECTION_NAME) } {
        // The target's own tag, read from its wrapper.
        let (target_root, target_path) = unsafe { section_parts(item) };
        let tag = {
            let tree = target_root.borrow();
            walk_read(&tree.data, &target_path)
                .map_or_else(String::new, |node| node.section_name().to_owned())
        };
        let (root, base) = unsafe { section_parts(self_) };
        let mut tree = root.borrow_mut();
        if let Some(node) = walk_mut(&mut tree.data, &base) {
            // The reference matches the child by section identity; the
            // port matches the first child of the name, and the answer is
            // true either way.
            node.del_child(&tag);
        }
        drop(tree);
        return boolean_object(true);
    }
    raise_type_error("py_deleteSection: Expected a DataSection\tor String argument.");
    std::ptr::null_mut()
}

/// `py_save` (`py_data_section.cpp:1894-1911`): write the tree back under
/// its origin id; a section with no file association fails with the
/// `IOError`.
///
/// # Safety
///
/// A C method call: the interpreter holds the GIL.
unsafe extern "C" fn method_save(
    self_: *mut capi::PyObject,
    args: *mut capi::PyObject,
) -> *mut capi::PyObject {
    if unsafe { capi::PyTuple_Size(args) } != 0 {
        raise_type_error("Expected no arguments.");
        return std::ptr::null_mut();
    }
    let (root, _path) = unsafe { section_parts(self_) };
    let origin = root.borrow().origin.clone();
    let Some(origin) = origin else {
        raise_io_error("Save failed");
        return std::ptr::null_mut();
    };
    let mut data = root.borrow().data.clone();
    let saved = store()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .resource
        .save_section(&origin, &mut data);
    if saved {
        capi::py_true()
    } else {
        raise_io_error("Save failed");
        std::ptr::null_mut()
    }
}

/// `py_copy` (`py_data_section.cpp:1921-1934`): another section's content
/// lands in this one; the name stays.
///
/// # Safety
///
/// A C method call: the interpreter holds the GIL.
unsafe extern "C" fn method_copy(
    self_: *mut capi::PyObject,
    args: *mut capi::PyObject,
) -> *mut capi::PyObject {
    let item = unsafe { capi::PyTuple_GetItem(args, 0) };
    if unsafe { capi::PyTuple_Size(args) } != 1 || unsafe { !check_type(item, DATA_SECTION_NAME) } {
        raise_type_error("Expected a PyDataSection.");
        return std::ptr::null_mut();
    }
    let (other_root, other_path) = unsafe { section_parts(item) };
    let source = {
        let tree = other_root.borrow();
        walk_read(&tree.data, &other_path).cloned()
    };
    let (root, base) = unsafe { section_parts(self_) };
    let mut tree = root.borrow_mut();
    if let (Some(node), Some(source)) = (walk_mut(&mut tree.data, &base), source) {
        node.copy_from(&source);
    }
    drop(tree);
    capi::py_none()
}

/// `py_copyToZip` (`py_data_section.cpp:1948-1958`): the XML base's
/// `convertToZip` answers the section unchanged, so the tuple is the
/// wrapper itself and `False`.
///
/// # Safety
///
/// A C method call: the interpreter holds the GIL.
unsafe extern "C" fn method_copy_to_zip(
    self_: *mut capi::PyObject,
    _args: *mut capi::PyObject,
) -> *mut capi::PyObject {
    let (root, path) = unsafe { section_parts(self_) };
    // SAFETY: the interpreter is up and the caller owns the GIL.
    let wrapper = unsafe { section_object(root, path) };
    let tuple = unsafe { capi::PyTuple_New(2) };
    // SAFETY: a fresh tuple steals the fresh objects.
    unsafe {
        capi::PyTuple_SetItem(tuple, 0, own_raw(wrapper));
        capi::PyTuple_SetItem(tuple, 1, boolean_object(false));
    }
    tuple
}

/// The `child` body (`py_data_section.cpp:1974-1992`): the indexed child,
/// the disappeared-child `EnvironmentError` (an orphaned wrapper carries
/// no name to quote) or the `IndexError`.
///
/// # Safety
///
/// The interpreter holds the GIL and `self_` is a section instance.
unsafe fn child_at(self_: *mut capi::PyObject, index: i32) -> *mut capi::PyObject {
    let (root, base) = unsafe { section_parts(self_) };
    let tree = root.borrow();
    let Some(section) = walk_read(&tree.data, &base) else {
        // The orphaned delta: the reference's engine child has gone away
        // under a still-live wrapper, so it raises the disappeared-child
        // `EnvironmentError` with no name to quote.
        drop(tree);
        raise_environment_error(&format!("child index {index} '' has disappeared"));
        return std::ptr::null_mut();
    };
    if !(0..section.count_children() as i64).contains(&(index as i64)) {
        drop(tree);
        raise_index_error("child index out of range");
        return std::ptr::null_mut();
    }
    let tag = section
        .open_child(index as usize)
        .map_or_else(String::new, |child| child.section_name().to_owned());
    drop(tree);
    // SAFETY: the interpreter is up and the caller owns the GIL.
    own_raw(unsafe { section_object(root, child_path(&base, &tag)) })
}

/// The `childName` body (`py_data_section.cpp:2008-2019`).
///
/// # Safety
///
/// The interpreter holds the GIL and `self_` is a section instance.
unsafe fn child_name_at(self_: *mut capi::PyObject, index: i32) -> *mut capi::PyObject {
    let (root, base) = unsafe { section_parts(self_) };
    let tree = root.borrow();
    let name = walk_read(&tree.data, &base).and_then(|section| {
        if (0..section.count_children() as i64).contains(&(index as i64)) {
            section
                .open_child(index as usize)
                .map(|child| child.section_name().to_owned())
        } else {
            None
        }
    });
    drop(tree);
    match name {
        Some(name) => string_object(&name),
        None => {
            raise_index_error("child index out of range");
            std::ptr::null_mut()
        }
    }
}

/// The auto-parsed `child` face (`py_data_section.hpp:88-89`, `ARG(int,
/// END)`).
///
/// # Safety
///
/// A C method call: the interpreter holds the GIL.
unsafe extern "C" fn method_child(
    self_: *mut capi::PyObject,
    args: *mut capi::PyObject,
) -> *mut capi::PyObject {
    if unsafe { capi::PyTuple_Size(args) } != 1 {
        arg_count_error("child", &["int"]);
        return std::ptr::null_mut();
    }
    let Some(index) = (unsafe { int_arg(args, 0) }) else {
        return std::ptr::null_mut();
    };
    unsafe { child_at(self_, index) }
}

/// The auto-parsed `childName` face.
///
/// # Safety
///
/// A C method call: the interpreter holds the GIL.
unsafe extern "C" fn method_child_name(
    self_: *mut capi::PyObject,
    args: *mut capi::PyObject,
) -> *mut capi::PyObject {
    if unsafe { capi::PyTuple_Size(args) } != 1 {
        arg_count_error("childName", &["int"]);
        return std::ptr::null_mut();
    }
    let Some(index) = (unsafe { int_arg(args, 0) }) else {
        return std::ptr::null_mut();
    };
    unsafe { child_name_at(self_, index) }
}

// ---------------------------------------------------------------------------
// PyDataSection: the type-dispatching write.
// ---------------------------------------------------------------------------

/// `py_write` (`py_data_section.cpp:1538-1613`): the parse wall, then the
/// value type picks the writer -- int, float, str, a sequence of the right
/// width, a `Math` vector or a matrix provider -- and anything else is the
/// tp_name error.
///
/// # Safety
///
/// A C method call: the interpreter holds the GIL.
unsafe extern "C" fn method_write(
    self_: *mut capi::PyObject,
    args: *mut capi::PyObject,
) -> *mut capi::PyObject {
    let count = unsafe { capi::PyTuple_Size(args) };
    let path = if count == 2 {
        unsafe { capi::PyTuple_GetItem(args, 0) }
    } else {
        std::ptr::null_mut()
    };
    if count != 2 || unsafe { !capi::py_str_check(path) } {
        raise_type_error("Expected string and an object.");
        return std::ptr::null_mut();
    }
    let value = unsafe { capi::PyTuple_GetItem(args, 1) };
    if unsafe { capi::py_long_check(value) } {
        return unsafe { write_value(self_, args, WriteKind::Int) };
    }
    if unsafe { capi::py_float_check(value) } {
        return unsafe { write_value(self_, args, WriteKind::Float) };
    }
    if unsafe { capi::py_str_check(value) } {
        return unsafe { write_value(self_, args, WriteKind::Str) };
    }
    if unsafe { capi::PySequence_Check(value) } != 0 {
        // SAFETY: the check above guarantees a sequence object.
        match unsafe { capi::PySequence_Size(value) } {
            3 => return unsafe { write_value(self_, args, WriteKind::Vec3) },
            2 => return unsafe { write_value(self_, args, WriteKind::Vec2) },
            4 => return unsafe { write_value(self_, args, WriteKind::Vec4) },
            // Other widths fall through to the class checks, exactly the
            // reference's shape.
            _ => {}
        }
    }
    for (name, kind) in [
        ("Math.Vector2", WriteKind::Vec2),
        ("Math.Vector3", WriteKind::Vec3),
        ("Math.Vector4", WriteKind::Vec4),
    ] {
        if unsafe { check_type(value, name) } {
            return unsafe { write_value(self_, args, kind) };
        }
    }
    // SAFETY: the probe runs under the GIL.
    if unsafe { matrix_of(value) }.is_some() {
        return unsafe { write_value(self_, args, WriteKind::Matrix) };
    }
    // SAFETY: the type object is live.
    let type_name = unsafe {
        let rendered = capi::PyType_GetName(capi::Py_TYPE(value));
        let text = capi::to_string(rendered);
        capi::dec_ref(rendered);
        text
    };
    raise_type_error(&format!(
        "PyDataSection.write: Unrecognised object type {type_name}"
    ));
    std::ptr::null_mut()
}

// ---------------------------------------------------------------------------
// ResTblStruct: the value reader of a ResourceTable.
// ---------------------------------------------------------------------------

/// Build a `ResTblStruct` instance over `(values, parent)`.
///
/// # Safety
///
/// The [`STRUCT_NAME`] type must be installed and the interpreter up.
unsafe fn struct_object(values: Option<StructValues>, parent: Option<PyRef>) -> PyRef {
    let address = type_address(STRUCT_NAME).expect("the table struct type is installed");
    let type_object = PyRef::borrowed(address as *mut capi::PyObject);
    // SAFETY: the type is one of this module's, and the interpreter is up.
    let instance = unsafe { binding::instantiate(&type_object) }
        .expect("the struct instance allocation cannot fail");
    // SAFETY: the instance is the fresh allocation this call owns; the
    // uninitialised body is written in one shot, never dropped as garbage.
    unsafe {
        std::ptr::write(
            body::<StructBody>(instance.as_ptr()),
            StructBody { values, parent },
        );
    }
    instance
}

/// The chain walk of `ResTblStruct::get` (`resource_table.cpp:167-196`):
/// the first struct on the parent chain whose values open the key answers
/// the guessed value; the miss is `None`.
///
/// # Safety
///
/// The interpreter holds the GIL and `start` is a `ResTblStruct` instance.
unsafe fn struct_chain_lookup(
    start: *mut capi::PyObject,
    key: &str,
) -> Option<*mut capi::PyObject> {
    let mut current = start;
    loop {
        // SAFETY: each body lives in an instance the chain keeps alive.
        let body_ = unsafe { body::<StructBody>(current) };
        // SAFETY: the body is alive under the GIL.
        let values = unsafe { (*body_).values.as_ref() };
        if let Some(values) = values {
            let tree = values.root.borrow();
            if let Some(node) = indexed_node(&tree.data, &values.indices)
                && let Some(section) = node.open_section(key)
            {
                // SAFETY: the interpreter is up and the caller owns
                // the GIL.
                let answer = unsafe { guess_python_from_section(section) };
                drop(tree);
                return Some(answer);
            }
        }
        // SAFETY: the parent handle is a live instance or absent.
        let parent = unsafe { (*body_).parent.as_ref() };
        {
            let parent = parent?;
            current = parent.as_ptr()
        }
    }
}

/// The `get` answer body: the chain lookup, else the caller's default --
/// inherited through an `inc_ref` like the reference's `PyObjectPtr`
/// answer -- else `None`.
///
/// # Safety
///
/// The interpreter holds the GIL; `self_` is a struct instance and
/// `default` a borrowed object or null.
unsafe fn struct_get_value(
    self_: *mut capi::PyObject,
    key: &str,
    default: *mut capi::PyObject,
) -> *mut capi::PyObject {
    if let Some(answer) = unsafe { struct_chain_lookup(self_, key) } {
        return answer;
    }
    if default.is_null() {
        return capi::py_none();
    }
    unsafe { capi::inc_ref(default) };
    default
}

/// The auto-parsed `get` face (`resource_table.hpp:38-40`, `ARG(BW::string,
/// OPTARG(PyObjectPtr, NULL, END))`).
///
/// # Safety
///
/// A C method call: the interpreter holds the GIL.
unsafe extern "C" fn method_struct_get(
    self_: *mut capi::PyObject,
    args: *mut capi::PyObject,
) -> *mut capi::PyObject {
    let count = unsafe { capi::PyTuple_Size(args) };
    if !(1..=2).contains(&count) {
        raise_type_error(&arg_count_between_sentence(
            "get",
            1,
            &["BW::string", "PyObjectPtr"],
        ));
        return std::ptr::null_mut();
    }
    let Some(key) = (unsafe { string_arg(args, 0, false) }) else {
        return std::ptr::null_mut();
    };
    let default = if count == 2 {
        let Some(object) = (unsafe { object_arg(args, 1) }) else {
            return std::ptr::null_mut();
        };
        object
    } else {
        std::ptr::null_mut()
    };
    unsafe { struct_get_value(self_, &key, default) }
}

/// `ResTblStruct::pyGetAttribute` (`resource_table.cpp:95-116`): a name
/// found anywhere on the parent chain answers through the chain, and
/// everything else falls to the generic walk.
///
/// # Safety
///
/// A C slot call: the interpreter holds the GIL.
unsafe extern "C" fn struct_getattro(
    self_: *mut capi::PyObject,
    name: *mut capi::PyObject,
) -> *mut capi::PyObject {
    if unsafe { capi::py_str_check(name) } {
        // SAFETY: the check above guarantees a str object.
        let key = unsafe { capi::to_string(name) };
        // SAFETY: self_ is a struct instance.
        if let Some(answer) = unsafe { struct_chain_lookup(self_, &key) } {
            return answer;
        }
    }
    unsafe { capi::PyObject_GenericGetAttr(self_, name) }
}

// ---------------------------------------------------------------------------
// ResourceTable: the deprecated resource table.
// ---------------------------------------------------------------------------

/// The wrapped node's text, the `pSect_->asString()` behind `key`/`sub`
/// (`resource_table.cpp:499-514`, `:559-562`).
fn table_node_text(table: &TableBody) -> Option<String> {
    let tree = table.root.borrow();
    indexed_node(&tree.data, &table.indices).map(|node| node.as_string().to_owned())
}

/// The `at` lookup (`resource_table.cpp:481-494`): a negative index wraps,
/// the wrapped position must land inside.
fn table_at_index(table: &TableBody, index: i32) -> Option<&PyRef> {
    let size = table.entries.len() as i64;
    let wrapped = if index >= 0 {
        index as i64
    } else {
        index as i64 + size
    };
    if wrapped < 0 || wrapped >= size {
        return None;
    }
    table.entries.get(wrapped as usize)
}

/// The `sub` lookup (`resource_table.cpp:499-514`): the entry whose node
/// text equals the key.
fn table_sub_key<'a>(table: &'a TableBody, key: &str) -> Option<&'a PyRef> {
    table.entries.iter().find(|entry| {
        // SAFETY: the entry is a live table instance.
        let entry_body = unsafe { &*body::<TableBody>(entry.as_ptr()) };
        table_node_text(entry_body).as_deref() == Some(key)
    })
}

/// The `at` answer core, shared by the method and the subscript.
///
/// # Safety
///
/// The interpreter holds the GIL and `self_` is a table instance.
unsafe fn table_at(self_: *mut capi::PyObject, index: i32) -> *mut capi::PyObject {
    // SAFETY: self_ is a table instance.
    let table = unsafe { &*body::<TableBody>(self_) };
    match table_at_index(table, index) {
        Some(entry) => {
            unsafe { capi::inc_ref(entry.as_ptr()) };
            entry.as_ptr()
        }
        None => {
            raise_value_error(&format!("ResourceTable.at: index {index} out of range"));
            std::ptr::null_mut()
        }
    }
}

/// The `sub` answer core.
///
/// # Safety
///
/// The interpreter holds the GIL and `self_` is a table instance.
unsafe fn table_sub(self_: *mut capi::PyObject, key: &str) -> *mut capi::PyObject {
    // SAFETY: self_ is a table instance.
    let table = unsafe { &*body::<TableBody>(self_) };
    match table_sub_key(table, key) {
        Some(entry) => {
            unsafe { capi::inc_ref(entry.as_ptr()) };
            entry.as_ptr()
        }
        None => {
            raise_value_error(&format!("ResourceTable.find: no entry for key {key}"));
            std::ptr::null_mut()
        }
    }
}

/// The auto-parsed `at` face (`resource_table.hpp:46-47`, `ARG(int, END)`).
///
/// # Safety
///
/// A C method call: the interpreter holds the GIL.
unsafe extern "C" fn method_at(
    self_: *mut capi::PyObject,
    args: *mut capi::PyObject,
) -> *mut capi::PyObject {
    if unsafe { capi::PyTuple_Size(args) } != 1 {
        arg_count_error("at", &["int"]);
        return std::ptr::null_mut();
    }
    let Some(index) = (unsafe { int_arg(args, 0) }) else {
        return std::ptr::null_mut();
    };
    unsafe { table_at(self_, index) }
}

/// The auto-parsed `sub` face (`resource_table.hpp:48-49`, `ARG(BW::string,
/// END)`).
///
/// # Safety
///
/// A C method call: the interpreter holds the GIL.
unsafe extern "C" fn method_sub(
    self_: *mut capi::PyObject,
    args: *mut capi::PyObject,
) -> *mut capi::PyObject {
    if unsafe { capi::PyTuple_Size(args) } != 1 {
        arg_count_error("sub", &["BW::string"]);
        return std::ptr::null_mut();
    }
    let Some(key) = (unsafe { string_arg(args, 0, false) }) else {
        return std::ptr::null_mut();
    };
    unsafe { table_sub(self_, &key) }
}

/// The auto-parsed `keyOfIndex` face (`resource_table.hpp:51-52`): the
/// entry's key text, the empty string with the `at` failure cleared
/// (`resource_table.cpp:521-534`).
///
/// # Safety
///
/// A C method call: the interpreter holds the GIL.
unsafe extern "C" fn method_key_of_index(
    self_: *mut capi::PyObject,
    args: *mut capi::PyObject,
) -> *mut capi::PyObject {
    if unsafe { capi::PyTuple_Size(args) } != 1 {
        arg_count_error("keyOfIndex", &["int"]);
        return std::ptr::null_mut();
    }
    let Some(index) = (unsafe { int_arg(args, 0) }) else {
        return std::ptr::null_mut();
    };
    // SAFETY: self_ is a table instance.
    let table = unsafe { &*body::<TableBody>(self_) };
    let key = table_at_index(table, index).map(|entry| {
        // SAFETY: the entry is a live table instance.
        let entry_body = unsafe { &*body::<TableBody>(entry.as_ptr()) };
        table_node_text(entry_body).unwrap_or_default()
    });
    match key {
        Some(key) => string_object(&key),
        None => {
            // SAFETY: the `at` failure leaves its error pending.
            unsafe { capi::clear_error() };
            string_object("")
        }
    }
}

/// The auto-parsed `indexOfKey` face (`resource_table.hpp:53-54`): the
/// entry's position, `-1` with the `sub` failure cleared
/// (`resource_table.cpp:540-553`).
///
/// # Safety
///
/// A C method call: the interpreter holds the GIL.
unsafe extern "C" fn method_index_of_key(
    self_: *mut capi::PyObject,
    args: *mut capi::PyObject,
) -> *mut capi::PyObject {
    if unsafe { capi::PyTuple_Size(args) } != 1 {
        arg_count_error("indexOfKey", &["BW::string"]);
        return std::ptr::null_mut();
    }
    let Some(key) = (unsafe { string_arg(args, 0, false) }) else {
        return std::ptr::null_mut();
    };
    // SAFETY: self_ is a table instance.
    let table = unsafe { &*body::<TableBody>(self_) };
    let index = table_sub_key(table, &key).map(|entry| {
        // SAFETY: the entry is a live table instance.
        let entry_body = unsafe { &*body::<TableBody>(entry.as_ptr()) };
        entry_body.index
    });
    match index {
        Some(index) => int_object(index as i64),
        None => {
            // SAFETY: the `sub` failure leaves its error pending.
            unsafe { capi::clear_error() };
            int_object(-1)
        }
    }
}

/// The `link` body (`resource_table.cpp:421-440`): insert into the link
/// set -- no duplicates, the reference's `BW::set` -- then the C++
/// `callNow` default runs the callable once with the table.
///
/// # Safety
///
/// The interpreter holds the GIL; `self_` is a table instance and
/// `update_fn` a live callable.
unsafe fn link_table(self_: *mut capi::PyObject, update_fn: *mut capi::PyObject) {
    // SAFETY: self_ is a table instance.
    let table = unsafe { &mut *body::<TableBody>(self_) };
    if !table.links.iter().any(|link| link.as_ptr() == update_fn) {
        table.links.push(PyRef::borrowed(update_fn));
    }
    let call_args = unsafe { capi::PyTuple_New(1) };
    unsafe {
        capi::inc_ref(self_);
        capi::PyTuple_SetItem(call_args, 0, self_);
        call_and_report(update_fn, call_args);
        capi::dec_ref(call_args);
    }
}

/// The auto-parsed `link` face (`resource_table.hpp:36-37`, `RETOK` with
/// `NZARG(PyObjectPtr, END)`).
///
/// # Safety
///
/// A C method call: the interpreter holds the GIL.
unsafe extern "C" fn method_link(
    self_: *mut capi::PyObject,
    args: *mut capi::PyObject,
) -> *mut capi::PyObject {
    if unsafe { capi::PyTuple_Size(args) } != 1 {
        arg_count_error("link", &["PyObjectPtr"]);
        return std::ptr::null_mut();
    }
    let Some(update_fn) = (unsafe { callable_arg(args, 0) }) else {
        return std::ptr::null_mut();
    };
    unsafe { link_table(self_, update_fn) };
    capi::py_true()
}

/// The auto-parsed `unlink` face (`resource_table.hpp:38-39`).
///
/// # Safety
///
/// A C method call: the interpreter holds the GIL.
unsafe extern "C" fn method_unlink(
    self_: *mut capi::PyObject,
    args: *mut capi::PyObject,
) -> *mut capi::PyObject {
    if unsafe { capi::PyTuple_Size(args) } != 1 {
        arg_count_error("unlink", &["PyObjectPtr"]);
        return std::ptr::null_mut();
    }
    let Some(update_fn) = (unsafe { object_arg(args, 0) }) else {
        return std::ptr::null_mut();
    };
    // SAFETY: self_ is a table instance.
    let table = unsafe { &mut *body::<TableBody>(self_) };
    table.links.retain(|link| link.as_ptr() != update_fn);
    capi::py_none()
}

/// The `value` getter (`resource_table.hpp:41`): the struct instance.
///
/// # Safety
///
/// A C getter call: the interpreter holds the GIL.
unsafe extern "C" fn get_table_value(
    self_: *mut capi::PyObject,
    _closure: *mut c_void,
) -> *mut capi::PyObject {
    // SAFETY: self_ is a table instance.
    let value = unsafe { (*body::<TableBody>(self_)).value.clone() };
    own_raw(value)
}

/// The `key` getter (`resource_table.hpp:57-58`): the wrapped node's own
/// text.
///
/// # Safety
///
/// A C getter call: the interpreter holds the GIL.
unsafe extern "C" fn get_table_key(
    self_: *mut capi::PyObject,
    _closure: *mut c_void,
) -> *mut capi::PyObject {
    // SAFETY: self_ is a table instance.
    let table = unsafe { &*body::<TableBody>(self_) };
    let text = table_node_text(table).unwrap_or_default();
    string_object(&text)
}

/// The `index` getter (`resource_table.hpp:56`): the root table keeps the
/// reference's `~0u`.
///
/// # Safety
///
/// A C getter call: the interpreter holds the GIL.
unsafe extern "C" fn get_table_index(
    self_: *mut capi::PyObject,
    _closure: *mut c_void,
) -> *mut capi::PyObject {
    // SAFETY: self_ is a table instance.
    let index = unsafe { (*body::<TableBody>(self_)).index };
    int_object(index as i64)
}

/// The mapping length (`resource_table.cpp:454-457`): the keyed entries.
///
/// # Safety
///
/// A C slot call: the interpreter holds the GIL and `self_` is an instance
/// of the type.
unsafe extern "C" fn rt_length(self_: *mut capi::PyObject) -> capi::PySsize {
    // SAFETY: self_ is a table instance.
    let table = unsafe { &*body::<TableBody>(self_) };
    table.entries.len() as capi::PySsize
}

/// The mapping subscript (`resource_table.cpp:462-476`): the int
/// conversion first -- its failure cleared -- then the string conversion
/// with the `"ResourceTable[x]: x"` name.
///
/// # Safety
///
/// A C slot call: the interpreter holds the GIL.
unsafe extern "C" fn rt_subscript(
    self_: *mut capi::PyObject,
    key: *mut capi::PyObject,
) -> *mut capi::PyObject {
    if unsafe { capi::py_long_check(key) } {
        // SAFETY: the check above guarantees an int object.
        let value = unsafe { capi::PyLong_AsLong(key) };
        if unsafe { capi::PyErr_Occurred() }.is_null() {
            return unsafe { table_at(self_, value as i32) };
        }
        // The int conversion failed: its error is cleared for the string
        // arm, the reference's `PyErr_Clear`.
        unsafe { capi::clear_error() };
    }
    if unsafe { capi::py_str_check(key) } {
        // SAFETY: the check above guarantees a str object.
        let key_text = unsafe { capi::to_string(key) };
        return unsafe { table_sub(self_, &key_text) };
    }
    raise_type_error("ResourceTable[x]: x must be set to a string");
    std::ptr::null_mut()
}

/// `ResourceTable::pyGetAttribute` (`resource_table.cpp:403-415`): the
/// value struct's own attributes first, its failure cleared so the error
/// text comes from the generic walk.
///
/// # Safety
///
/// A C slot call: the interpreter holds the GIL.
unsafe extern "C" fn rt_getattro(
    self_: *mut capi::PyObject,
    name: *mut capi::PyObject,
) -> *mut capi::PyObject {
    // SAFETY: self_ is a table instance.
    let value = unsafe { &(*body::<TableBody>(self_)).value };
    // SAFETY: the value instance is live under the GIL.
    let try_answer = unsafe { capi::PyObject_GetAttr(value.as_ptr(), name) };
    if !try_answer.is_null() {
        return try_answer;
    }
    // SAFETY: the failed probe leaves its error pending.
    unsafe { capi::clear_error() };
    unsafe { capi::PyObject_GenericGetAttr(self_, name) }
}

/// Build a `ResourceTable` instance over the tree at `indices`, with
/// `parent_value` as the value chain's tail -- the constructor
/// (`resource_table.cpp:330-389`): the value struct first, then one
/// sub-table per `key` child, each indexed by its push position.
///
/// # Safety
///
/// The [`TABLE_NAME`] and [`STRUCT_NAME`] types must be installed and the
/// interpreter up.
unsafe fn build_table(
    root: TreeHandle,
    indices: Vec<usize>,
    parent_value: Option<PyRef>,
    index: u32,
) -> PyRef {
    // The `value` child, or the reference's null values when absent.
    let values = {
        let tree = root.borrow();
        indexed_node(&tree.data, &indices).and_then(|node| {
            (0..node.count_children())
                .find(|&position| {
                    node.open_child(position)
                        .is_some_and(|child| child.section_name() == "value")
                })
                .map(|position| {
                    let mut value_indices = indices.clone();
                    value_indices.push(position);
                    StructValues {
                        root: root.clone(),
                        indices: value_indices,
                    }
                })
        })
    };
    // SAFETY: both types are installed under the GIL.
    let value = unsafe { struct_object(values, parent_value) };
    let mut entries = Vec::new();
    {
        let tree = root.borrow();
        let key_positions: Vec<usize> =
            indexed_node(&tree.data, &indices).map_or_else(Vec::new, |node| {
                (0..node.count_children())
                    .filter(|&position| {
                        node.open_child(position)
                            .is_some_and(|child| child.section_name() == "key")
                    })
                    .collect()
            });
        for position in key_positions {
            let mut child_indices = indices.clone();
            child_indices.push(position);
            // SAFETY: the recursion runs under the same contract.
            let entry = unsafe {
                build_table(
                    root.clone(),
                    child_indices,
                    Some(value.clone()),
                    entries.len() as u32,
                )
            };
            entries.push(entry);
        }
    }
    let address = type_address(TABLE_NAME).expect("the table type is installed");
    let type_object = PyRef::borrowed(address as *mut capi::PyObject);
    // SAFETY: the type is one of this module's, and the interpreter is up.
    let instance = unsafe { binding::instantiate(&type_object) }
        .expect("the table instance allocation cannot fail");
    // SAFETY: the instance is the fresh allocation this call owns; the
    // uninitialised body is written in one shot, never dropped as garbage.
    unsafe {
        std::ptr::write(
            body::<TableBody>(instance.as_ptr()),
            TableBody {
                root,
                indices,
                value,
                entries,
                index,
                links: Vec::new(),
            },
        );
    }
    instance
}

/// The `ResourceTable` factory on the type itself --
/// `PY_AUTO_FACTORY_DECLARE` (`resource_table.hpp:29-32`) makes the class
/// call `New` (`resource_table.cpp:584-634`): the auto parse, the
/// deprecation warning, the callable wall, the engine open and the
/// id-keyed census.
///
/// # Safety
///
/// A C `tp_new` call: the interpreter holds the GIL.
unsafe extern "C" fn new_resource_table(
    _type_object: *mut capi::PyTypeObject,
    args: *mut capi::PyObject,
    _kwds: *mut capi::PyObject,
) -> *mut capi::PyObject {
    let count = unsafe { capi::PyTuple_Size(args) };
    if !(1..=2).contains(&count) {
        raise_type_error(&arg_count_between_sentence(
            "ResourceTable",
            1,
            &["BW::string", "PyObjectPtr"],
        ));
        return std::ptr::null_mut();
    }
    let Some(resource_id) = (unsafe { string_arg(args, 0, false) }) else {
        return std::ptr::null_mut();
    };
    let update_fn = if count == 2 {
        let Some(object) = (unsafe { object_arg(args, 1) }) else {
            return std::ptr::null_mut();
        };
        Some(object)
    } else {
        None
    };
    // WARNING_MSG, rendered as a stderr line per the pinned choice.
    eprintln!(
        "ResourceTable has been deprecated and is planned for removal in \
         the next BigWorld release.\nUse DataSection support instead."
    );
    if let Some(update_fn) = update_fn
        && unsafe { capi::PyCallable_Check(update_fn) } == 0
    {
        raise_type_error("ResourceTable() second arg must be callable");
        return std::ptr::null_mut();
    }
    let mut store_state = store()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let Some(tree) = census_tree(&mut store_state, &resource_id, false) else {
        raise_value_error(&format!("ResourceTable() no such resource '{resource_id}'"));
        return std::ptr::null_mut();
    };
    if let Some(existing) = store_state.tables.get(&resource_id) {
        let existing = existing.clone();
        drop(store_state);
        if let Some(update_fn) = update_fn {
            // SAFETY: the census table is a live instance.
            unsafe { link_table(existing.as_ptr(), update_fn) };
        }
        return own_raw(existing);
    }
    // SAFETY: both table types are installed under the GIL.
    let table = unsafe { build_table(tree, Vec::new(), None, u32::MAX) };
    store_state
        .tables
        .insert(resource_id.clone(), table.clone());
    drop(store_state);
    if let Some(update_fn) = update_fn {
        // SAFETY: the fresh table is a live instance.
        unsafe { link_table(table.as_ptr(), update_fn) };
    }
    own_raw(table)
}

// ---------------------------------------------------------------------------
// The ResMgr module functions (`res_mgr_script.cpp:45-329`).
// ---------------------------------------------------------------------------

/// `isDir` (`res_mgr_script.cpp:45-49`, `ARG(BW::string, END)` `RETDATA`).
///
/// # Safety
///
/// A C function call: the interpreter holds the GIL.
unsafe extern "C" fn fn_is_dir(
    _module: *mut capi::PyObject,
    args: *mut capi::PyObject,
) -> *mut capi::PyObject {
    if unsafe { capi::PyTuple_Size(args) } != 1 {
        arg_count_error("isDir", &["BW::string"]);
        return std::ptr::null_mut();
    }
    let Some(path) = (unsafe { string_arg(args, 0, false) }) else {
        return std::ptr::null_mut();
    };
    let answer = store()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .resource
        .is_dir(&path);
    boolean_object(answer)
}

/// `isFile` (`res_mgr_script.cpp:65-69`).
///
/// # Safety
///
/// A C function call: the interpreter holds the GIL.
unsafe extern "C" fn fn_is_file(
    _module: *mut capi::PyObject,
    args: *mut capi::PyObject,
) -> *mut capi::PyObject {
    if unsafe { capi::PyTuple_Size(args) } != 1 {
        arg_count_error("isFile", &["BW::string"]);
        return std::ptr::null_mut();
    }
    let Some(path) = (unsafe { string_arg(args, 0, false) }) else {
        return std::ptr::null_mut();
    };
    let answer = store()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .resource
        .is_file(&path);
    boolean_object(answer)
}

/// `openSection` (`res_mgr_script.cpp:92-115`, `ARG(BW::string,
/// OPTARG(bool, false, END))` `RETOWN`): the live tree census, a `None`
/// miss, the `ValueError` refusal for a section the engine could not make.
///
/// # Safety
///
/// A C function call: the interpreter holds the GIL.
unsafe extern "C" fn fn_open_section(
    _module: *mut capi::PyObject,
    args: *mut capi::PyObject,
) -> *mut capi::PyObject {
    let count = unsafe { capi::PyTuple_Size(args) };
    if !(1..=2).contains(&count) {
        raise_type_error(&arg_count_between_sentence(
            "openSection",
            1,
            &["BW::string", "bool"],
        ));
        return std::ptr::null_mut();
    }
    let Some(resource_id) = (unsafe { string_arg(args, 0, false) }) else {
        return std::ptr::null_mut();
    };
    let make_new = if count == 2 {
        let Some(make_new) = (unsafe { bool_arg(args, 1, true) }) else {
            return std::ptr::null_mut();
        };
        make_new
    } else {
        false
    };
    let mut store_state = store()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let tree = census_tree(&mut store_state, &resource_id, make_new);
    drop(store_state);
    match tree {
        Some(tree) => {
            // SAFETY: the interpreter is up and the caller owns the GIL.
            own_raw(unsafe { section_object(tree, String::new()) })
        }
        None if make_new => {
            raise_value_error(&format!(
                "ResMgr.openSection(): Could not make new section '{resource_id}'"
            ));
            std::ptr::null_mut()
        }
        None => capi::py_none(),
    }
}

/// `save` (`res_mgr_script.cpp:132-139`, `RETOK`): the live tree of an
/// open id writes back; an id never opened fails with the `IOError`.
///
/// # Safety
///
/// A C function call: the interpreter holds the GIL.
unsafe extern "C" fn fn_save(
    _module: *mut capi::PyObject,
    args: *mut capi::PyObject,
) -> *mut capi::PyObject {
    if unsafe { capi::PyTuple_Size(args) } != 1 {
        arg_count_error("save", &["BW::string"]);
        return std::ptr::null_mut();
    }
    let Some(resource_id) = (unsafe { string_arg(args, 0, false) }) else {
        return std::ptr::null_mut();
    };
    let store_state = store()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let saved = match store_state.sections.get(&resource_id) {
        Some(tree) => {
            let mut tree = tree.borrow_mut();
            store_state
                .resource
                .save_section(&resource_id, &mut tree.data)
        }
        None => false,
    };
    drop(store_state);
    if saved {
        capi::py_true()
    } else {
        raise_io_error(&format!("Save of {resource_id} failed"));
        std::ptr::null_mut()
    }
}

/// `purge` (`res_mgr_script.cpp:158-163`, `RETVOID`): the engine cache and
/// the port's tree census both drop the id, so a re-open re-reads the
/// file.
///
/// # Safety
///
/// A C function call: the interpreter holds the GIL.
unsafe extern "C" fn fn_purge(
    _module: *mut capi::PyObject,
    args: *mut capi::PyObject,
) -> *mut capi::PyObject {
    let count = unsafe { capi::PyTuple_Size(args) };
    if !(1..=2).contains(&count) {
        raise_type_error(&arg_count_between_sentence(
            "purge",
            1,
            &["BW::string", "bool"],
        ));
        return std::ptr::null_mut();
    }
    let Some(resource_id) = (unsafe { string_arg(args, 0, false) }) else {
        return std::ptr::null_mut();
    };
    let recurse = if count == 2 {
        let Some(recurse) = (unsafe { bool_arg(args, 1, true) }) else {
            return std::ptr::null_mut();
        };
        recurse
    } else {
        false
    };
    let mut store_state = store()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let directory = recurse && store_state.resource.is_dir(&resource_id);
    store_state.resource.purge(&resource_id, recurse);
    // The recursive purge walks the directory (`bwresource.cpp` `BWResource::purge`),
    // so the census drops every id under it, not just the path named.
    let prefix = format!("{resource_id}/");
    store_state
        .sections
        .retain(|id, _| !id.starts_with(&prefix) || !directory);
    drop(store_state);
    capi::py_none()
}

/// `resolveToAbsolutePath` (`res_mgr_script.cpp:271-290`): the engine
/// resolution, with the first resource path as the not-found fallback, and
/// the whole answer normalised.
///
/// # Safety
///
/// A C function call: the interpreter holds the GIL.
unsafe extern "C" fn fn_resolve_to_absolute_path(
    _module: *mut capi::PyObject,
    args: *mut capi::PyObject,
) -> *mut capi::PyObject {
    if unsafe { capi::PyTuple_Size(args) } != 1 {
        arg_count_error("resolveToAbsolutePath", &["BW::string"]);
        return std::ptr::null_mut();
    }
    let Some(path) = (unsafe { string_arg(args, 0, false) }) else {
        return std::ptr::null_mut();
    };
    let store_state = store()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let mut resolved = path.clone();
    if store_state.resource.resolve_to_absolute_path(&mut resolved)
        == resources::zip::FileType::NotFound
    {
        resolved = if is_absolute_path(&path) {
            path.clone()
        } else {
            format!("{}/{}", store_state.resource.get_path(0), path)
        };
    }
    drop(store_state);
    string_object(&normalise_path(&resolved))
}

/// `localise` (`res_mgr_script.cpp:307-329`): the reference's own
/// `"s|ssssssss"` parse, and the `LocaliseUTF8` lookup -- which with no
/// localisation data in scope answers the key unchanged.
///
/// # Safety
///
/// A C function call: the interpreter holds the GIL.
unsafe extern "C" fn fn_localise(
    _module: *mut capi::PyObject,
    args: *mut capi::PyObject,
) -> *mut capi::PyObject {
    let mut key: *const c_char = std::ptr::null();
    let mut params: [*const c_char; 8] = [std::ptr::null(); 8];
    // SAFETY: the parse borrows only the arguments tuple and the out
    // pointers below, all of which outlive the call; the slots go by
    // raw pointer because every `&mut params[i]` would borrow the whole
    // array at once.
    let slots = params.as_mut_ptr();
    let ok = unsafe {
        capi::PyArg_ParseTuple(
            args,
            c"s|ssssssss".as_ptr(),
            &mut key,
            slots,
            slots.add(1),
            slots.add(2),
            slots.add(3),
            slots.add(4),
            slots.add(5),
            slots.add(6),
            slots.add(7),
        )
    };
    if ok == 0 {
        // The parse failure is the reference's own `TypeError`.
        return std::ptr::null_mut();
    }
    // SAFETY: the parse left a NUL-terminated borrowed pointer.
    let text = unsafe { std::ffi::CStr::from_ptr(key) }
        .to_string_lossy()
        .into_owned();
    string_object(&text)
}

// ---------------------------------------------------------------------------
// The editor trio (`res_mgr_script.cpp:166-260`, `#ifdef EDITOR_ENABLED`).
// ---------------------------------------------------------------------------

/// `remove` (`res_mgr_script.cpp:181-191`, `RETDATA`).
///
/// # Safety
///
/// A C function call: the interpreter holds the GIL.
unsafe extern "C" fn fn_remove(
    _module: *mut capi::PyObject,
    args: *mut capi::PyObject,
) -> *mut capi::PyObject {
    if unsafe { capi::PyTuple_Size(args) } != 1 {
        arg_count_error("remove", &["BW::string"]);
        return std::ptr::null_mut();
    }
    let Some(res_id) = (unsafe { string_arg(args, 0, false) }) else {
        return std::ptr::null_mut();
    };
    let removed = store()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .resource
        .file_system()
        .erase_file_or_directory(&res_id);
    if removed {
        boolean_object(true)
    } else {
        raise_io_error(&format!("Removing of {res_id} failed"));
        std::ptr::null_mut()
    }
}

/// `rename` (`res_mgr_script.cpp:207-221`).
///
/// # Safety
///
/// A C function call: the interpreter holds the GIL.
unsafe extern "C" fn fn_rename(
    _module: *mut capi::PyObject,
    args: *mut capi::PyObject,
) -> *mut capi::PyObject {
    if unsafe { capi::PyTuple_Size(args) } != 2 {
        arg_count_error("rename", &["BW::string", "BW::string"]);
        return std::ptr::null_mut();
    }
    let (Some(old_id), Some(new_id)) = (unsafe { string_arg(args, 0, false) }, unsafe {
        string_arg(args, 1, false)
    }) else {
        return std::ptr::null_mut();
    };
    let renamed = store()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .resource
        .file_system()
        .move_file_or_directory(&old_id, &new_id);
    if renamed {
        boolean_object(true)
    } else {
        raise_io_error(&format!("Renaming of {old_id} to {new_id} failed"));
        std::ptr::null_mut()
    }
}

/// `copy` (`res_mgr_script.cpp:237-257`): a read and a write through the
/// file system.
///
/// # Safety
///
/// A C function call: the interpreter holds the GIL.
unsafe extern "C" fn fn_copy(
    _module: *mut capi::PyObject,
    args: *mut capi::PyObject,
) -> *mut capi::PyObject {
    if unsafe { capi::PyTuple_Size(args) } != 2 {
        arg_count_error("copy", &["BW::string", "BW::string"]);
        return std::ptr::null_mut();
    }
    let (Some(source_id), Some(dest_id)) = (unsafe { string_arg(args, 0, false) }, unsafe {
        string_arg(args, 1, false)
    }) else {
        return std::ptr::null_mut();
    };
    let copied = {
        let store_state = store()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        store_state
            .resource
            .file_system()
            .read_file(&source_id)
            .is_some_and(|data| {
                store_state
                    .resource
                    .file_system()
                    .write_file(&dest_id, &data)
            })
    };
    if copied {
        boolean_object(true)
    } else {
        raise_io_error(&format!("Copying of {source_id} to {dest_id} failed"));
        std::ptr::null_mut()
    }
}

/// The function table every component installs (`res_mgr_script.cpp`):
/// the `PY_AUTO_MODULE_FUNCTION` links. A newtype because a static slice
/// of `PyMethodDef` is not `Sync` over the raw `ml_name` pointers, but
/// these tables are immutable constants the module build only reads.
struct MethodTable(&'static [capi::PyMethodDef]);
unsafe impl Sync for MethodTable {}

static BOTH_FUNCTIONS: MethodTable = MethodTable(&[
    capi::PyMethodDef {
        ml_name: c"isDir".as_ptr(),
        ml_meth: Some(fn_is_dir),
        ml_flags: capi::METH_VARARGS,
        ml_doc: std::ptr::null(),
    },
    capi::PyMethodDef {
        ml_name: c"isFile".as_ptr(),
        ml_meth: Some(fn_is_file),
        ml_flags: capi::METH_VARARGS,
        ml_doc: std::ptr::null(),
    },
    capi::PyMethodDef {
        ml_name: c"openSection".as_ptr(),
        ml_meth: Some(fn_open_section),
        ml_flags: capi::METH_VARARGS,
        ml_doc: std::ptr::null(),
    },
    capi::PyMethodDef {
        ml_name: c"save".as_ptr(),
        ml_meth: Some(fn_save),
        ml_flags: capi::METH_VARARGS,
        ml_doc: std::ptr::null(),
    },
    capi::PyMethodDef {
        ml_name: c"purge".as_ptr(),
        ml_meth: Some(fn_purge),
        ml_flags: capi::METH_VARARGS,
        ml_doc: std::ptr::null(),
    },
    capi::PyMethodDef {
        ml_name: c"resolveToAbsolutePath".as_ptr(),
        ml_meth: Some(fn_resolve_to_absolute_path),
        ml_flags: capi::METH_VARARGS,
        ml_doc: std::ptr::null(),
    },
    capi::PyMethodDef {
        ml_name: c"localise".as_ptr(),
        ml_meth: Some(fn_localise),
        ml_flags: capi::METH_VARARGS,
        ml_doc: std::ptr::null(),
    },
    capi::PyMethodDef::END,
]);

/// The `#ifdef EDITOR_ENABLED` trio, an `editor` component's table.
static TOOL_FUNCTIONS: MethodTable = MethodTable(&[
    capi::PyMethodDef {
        ml_name: c"remove".as_ptr(),
        ml_meth: Some(fn_remove),
        ml_flags: capi::METH_VARARGS,
        ml_doc: std::ptr::null(),
    },
    capi::PyMethodDef {
        ml_name: c"rename".as_ptr(),
        ml_meth: Some(fn_rename),
        ml_flags: capi::METH_VARARGS,
        ml_doc: std::ptr::null(),
    },
    capi::PyMethodDef {
        ml_name: c"copy".as_ptr(),
        ml_meth: Some(fn_copy),
        ml_flags: capi::METH_VARARGS,
        ml_doc: std::ptr::null(),
    },
    capi::PyMethodDef::END,
]);

// ---------------------------------------------------------------------------
// The module installation and teardown.
// ---------------------------------------------------------------------------

/// Attach one created type to the module dictionary under its bare name --
/// the `PyFactoryMethodLink` step (`py_factory_method_link.cpp:37-80`),
/// which also rewrites `tp_name` to the qualified form the spec already
/// carries.
///
/// # Safety
///
/// The interpreter must be up and `module` a live module object.
unsafe fn attach(module: *mut capi::PyObject, name: &std::ffi::CStr, object: &PyRef) {
    // SAFETY: the module dictionary is live and the name is a
    // NUL-terminated key.
    unsafe {
        capi::PyDict_SetItemString(
            capi::PyModule_GetDict(module),
            name.as_ptr(),
            object.as_ptr(),
        );
    }
}

/// Build the whole `ResMgr` module: the three types -- `DataSection` and
/// `ResourceTable` linked into the module, `ResTblStruct` only ever built
/// by a table -- the function tables of the component, and the `root`
/// attribute. Idempotent per process: a repeat install re-attaches what
/// exists.
///
/// # Safety
///
/// The interpreter must be up on the calling thread.
pub unsafe fn install(component: &str) {
    let module = unsafe { capi::PyImport_AddModule(c"ResMgr".as_ptr()) };
    assert!(
        !module.is_null(),
        "ResMgr: the module could not be created or found"
    );
    // SAFETY: the interpreter is up and this thread owns the GIL.
    unsafe {
        let spec = with_dealloc(
            TypeSpec::new(DATA_SECTION_NAME)
                .instance_size(std::mem::size_of::<SectionBody>())
                .slot(capi::Py_tp_new, new_data_section as *mut c_void)
                .slot(capi::Py_mp_length, ds_length as *mut c_void)
                .slot(capi::Py_mp_subscript, ds_subscript as *mut c_void)
                .slot(capi::Py_tp_getattro, ds_getattro as *mut c_void)
                .slot(capi::Py_tp_setattro, ds_setattro as *mut c_void)
                .method("has_key", method_has_key)
                .method("keys", method_keys)
                .method("items", method_items)
                .method("values", method_values)
                .method("readString", method_read_string)
                .method("readWideString", method_read_wide_string)
                .method("readInt", method_read_int)
                .method("readInt64", method_read_int64)
                .method("readFloat", method_read_float)
                .method("readVector2", method_read_vector2)
                .method("readVector3", method_read_vector3)
                .method("readVector4", method_read_vector4)
                .method("readMatrix", method_read_matrix)
                .method("readBool", method_read_bool)
                .method("readBlob", method_read_blob)
                .method("writeString", method_write_string)
                .method("writeWideString", method_write_wide_string)
                .method("writeInt", method_write_int)
                .method("writeInt64", method_write_int64)
                .method("writeFloat", method_write_float)
                .method("writeVector2", method_write_vector2)
                .method("writeVector3", method_write_vector3)
                .method("writeVector4", method_write_vector4)
                .method("writeMatrix", method_write_matrix)
                .method("writeBool", method_write_bool)
                .method("writeBlob", method_write_blob)
                .method("write", method_write)
                .method("cleanName", method_clean_name)
                .method("isNameClean", method_is_name_clean)
                .method("readStrings", method_read_strings)
                .method("readWideStrings", method_read_wide_strings)
                .method("readInts", method_read_ints)
                .method("readFloats", method_read_floats)
                .method("readVector2s", method_read_vector2s)
                .method("readVector3s", method_read_vector3s)
                .method("readVector4s", method_read_vector4s)
                .method("writeStrings", method_write_strings)
                .method("writeWideStrings", method_write_wide_strings)
                .method("writeInts", method_write_ints)
                .method("writeFloats", method_write_floats)
                .method("writeVector2s", method_write_vector2s)
                .method("writeVector3s", method_write_vector3s)
                .method("writeVector4s", method_write_vector4s)
                .method("copyToZip", method_copy_to_zip)
                .method("createSection", method_create_section)
                .method("createSectionFromString", method_create_section_from_string)
                .method("deleteSection", method_delete_section)
                .method("save", method_save)
                .method("copy", method_copy)
                .method("child", method_child)
                .method("childName", method_child_name)
                .attribute("isAttribute", get_is_attribute, Some(set_is_attribute))
                .attribute("asString", get_as_string, Some(set_as_string))
                .attribute("asWideString", get_as_wide_string, Some(set_as_wide_string))
                .attribute("asInt", get_as_int, Some(set_as_int))
                .attribute("asInt64", get_as_int64, Some(set_as_int64))
                .attribute("asFloat", get_as_float, Some(set_as_float))
                .attribute("asVector2", get_as_vector2, Some(set_as_vector2))
                .attribute("asVector3", get_as_vector3, Some(set_as_vector3))
                .attribute("asVector4", get_as_vector4, Some(set_as_vector4))
                .attribute("asMatrix", get_as_matrix, Some(set_as_matrix))
                .attribute("asBool", get_as_bool, Some(set_as_bool))
                .attribute("asBinary", get_as_binary, Some(set_as_binary))
                .attribute("asBlob", get_as_blob, Some(set_as_blob))
                .attribute("name", get_name, None),
        );
        let data_section = match binding::create_type(&spec) {
            Ok(object) => object,
            Err(_) => panic!("ResMgr: the {DATA_SECTION_NAME} type could not be created"),
        };
        remember_type(DATA_SECTION_NAME, &data_section);
        register_teardown::<SectionBody>(data_section.as_ptr() as usize);
        attach(module, c"DataSection", &data_section);
        drop(data_section);

        let spec = with_dealloc(
            TypeSpec::new(STRUCT_NAME)
                .instance_size(std::mem::size_of::<StructBody>())
                .slot(capi::Py_tp_getattro, struct_getattro as *mut c_void)
                .method("get", method_struct_get),
        );
        let table_struct = match binding::create_type(&spec) {
            Ok(object) => object,
            Err(_) => panic!("ResMgr: the {STRUCT_NAME} type could not be created"),
        };
        remember_type(STRUCT_NAME, &table_struct);
        register_teardown::<StructBody>(table_struct.as_ptr() as usize);
        // The struct type never links into the module -- it only exists as
        // a `ResourceTable.value`.
        drop(table_struct);

        let spec = with_dealloc(
            TypeSpec::new(TABLE_NAME)
                .instance_size(std::mem::size_of::<TableBody>())
                .slot(capi::Py_tp_new, new_resource_table as *mut c_void)
                .slot(capi::Py_mp_length, rt_length as *mut c_void)
                .slot(capi::Py_mp_subscript, rt_subscript as *mut c_void)
                .slot(capi::Py_tp_getattro, rt_getattro as *mut c_void)
                .method("link", method_link)
                .method("unlink", method_unlink)
                .method("at", method_at)
                .method("sub", method_sub)
                .method("keyOfIndex", method_key_of_index)
                .method("indexOfKey", method_index_of_key)
                .attribute("value", get_table_value, None)
                .attribute("key", get_table_key, None)
                .attribute("index", get_table_index, None),
        );
        let table = match binding::create_type(&spec) {
            Ok(object) => object,
            Err(_) => panic!("ResMgr: the {TABLE_NAME} type could not be created"),
        };
        remember_type(TABLE_NAME, &table);
        register_teardown::<TableBody>(table.as_ptr() as usize);
        attach(module, c"ResourceTable", &table);
        drop(table);

        // SAFETY: the module object is live and the tables are static
        // storage the call only reads.
        capi::PyModule_AddFunctions(module, BOTH_FUNCTIONS.0.as_ptr());
        if component == "editor" {
            // The `#ifdef EDITOR_ENABLED` trio, rendered as the role test.
            capi::PyModule_AddFunctions(module, TOOL_FUNCTIONS.0.as_ptr());
        }

        // `root`: the engine's root section, an empty in-memory tree in
        // the port until the directory-section task lands.
        let root = section_object(memory_tree(DataSection::new("")), String::new());
        // SAFETY: the module object is live and the call steals the
        // wrapper's reference.
        capi::PyModule_AddObject(module, c"root".as_ptr(), own_raw(root));
    }
}

/// The fini-time teardown: release the censuses while the allocator is
/// still alive, then clear the registries -- the same shape as the math
/// module's reset, ahead of `binding::reset` releasing the types.
pub fn reset() {
    {
        let mut store_state = store()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        store_state.tables.clear();
        store_state.sections.clear();
    }
    teardowns()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clear();
    types()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clear();
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The normalise rules: separators fold, empties and "." drop, `x/..`
    /// folds, a leading `..` stays, a leading separator is kept, and
    /// nothing left answers ".".
    #[test]
    fn the_paths_normalise_like_bw_util() {
        assert_eq!(normalise_path(""), ".");
        assert_eq!(normalise_path("a/b/../c"), "a/c");
        assert_eq!(normalise_path("./a//b\\c"), "a/b/c");
        assert_eq!(normalise_path("/a/../.."), "/..");
        assert_eq!(normalise_path("a/.."), ".");
    }

    /// The absolute test: a leading separator or a `X:` drive.
    #[test]
    fn the_absolute_path_test_reads_like_bw_util() {
        assert!(is_absolute_path("/a"));
        assert!(is_absolute_path("\\a"));
        assert!(is_absolute_path("C:/a"));
        assert!(!is_absolute_path("a/b"));
        assert!(!is_absolute_path("c"));
    }

    /// The auto varNames the wrong-type sentences quote.
    #[test]
    fn the_arg_var_names_read_like_the_auto_machinery() {
        assert_eq!(arg_var_name(1, false), "() argument 1");
        assert_eq!(arg_var_name(2, true), "() argument 2 optionally");
    }

    /// The `createSection` path split: the dirname with "." cleared and a
    /// trailing separator dropped, the leaf name kept.
    #[test]
    fn the_create_section_path_split_reads_like_the_reference() {
        assert_eq!(
            split_file_path("a/b/c.xml"),
            ("a/b".to_owned(), "c.xml".to_owned())
        );
        assert_eq!(
            split_file_path("c.xml"),
            (String::new(), "c.xml".to_owned())
        );
        assert_eq!(
            split_file_path("/c.xml"),
            (String::new(), "c.xml".to_owned())
        );
    }

    /// The child path join of the wrapper model.
    #[test]
    fn the_child_path_joins_with_slashes() {
        assert_eq!(child_path("", "a"), "a");
        assert_eq!(child_path("a/b", "c"), "a/b/c");
    }

    /// The index walk resolves positional chains and misses past the end.
    #[test]
    fn the_indexed_walk_resolves_positions() {
        let mut root = DataSection::new("root");
        let child = root.add_child("a");
        child.add_child("b").set_string("inner");
        assert_eq!(
            indexed_node(&root, &[0, 0]).map(|node| node.as_string()),
            Some("inner")
        );
        assert!(indexed_node(&root, &[0, 1]).is_none());
        assert!(indexed_node(&root, &[1]).is_none());
    }

    /// The tag split of the read*s: the parent node and the leaf tag.
    #[test]
    fn the_tag_split_reads_the_parent_and_leaf() {
        let mut root = DataSection::new("root");
        root.add_child("a").add_child("b").set_string("inner");
        let (parent, leaf) = split_tag_read(&root, "a/b").expect("the walk resolves");
        assert_eq!(leaf, "b");
        // The base walk lands on the "a" node; the "b" leaf is a child of
        // it. The base node itself carries no text -- the read*s iterate
        // its children and read the value off each matching child.
        assert_eq!(parent.section_name(), "a");
        assert_eq!(
            parent.open_section("b").map(|node| node.as_string()),
            Some("inner")
        );
        // A missing leaf still resolves: only the base is walked here, and
        // the read*s find no matching child later.
        let (parent, leaf) = split_tag_read(&root, "a/c").expect("the base resolves");
        assert_eq!(leaf, "c");
        assert_eq!(parent.section_name(), "a");
        // A missing base is the None case.
        assert!(split_tag_read(&root, "x/c").is_none());
    }

    /// The make-new tag split of the write*s: missing parents are created
    /// on the way.
    #[test]
    fn the_tag_split_makes_missing_parents() {
        let mut root = DataSection::new("root");
        let (parent, leaf) = split_tag_path(&mut root, "a/b", true).expect("the parents are made");
        assert_eq!(leaf, "b");
        assert_eq!(parent.section_name(), "a");
        assert!(split_tag_path(&mut root, "c/d", false).is_none());
    }

    /// The child tags, in document order.
    #[test]
    fn the_child_tags_keep_document_order() {
        let mut root = DataSection::new("root");
        root.add_child("b");
        root.add_child("a");
        root.add_child("b");
        assert_eq!(child_tags(&root), ["b", "a", "b"]);
    }

    /// The matrix read: the `rowN` children over a zero default, column 3
    /// forced to `0,0,0,1` (`xml_section.cpp:915-935`).
    #[test]
    fn the_matrix_read_builds_the_rows() {
        let mut section = DataSection::new("m");
        section
            .add_child("row0")
            .set_vector3(Vector3::new(1.0, 0.0, 0.0));
        section
            .add_child("row1")
            .set_vector3(Vector3::new(0.0, 1.0, 0.0));
        let value = matrix_from_section(&section, MatrixValue::zero());
        assert_eq!(value.m[0][0], 1.0);
        assert_eq!(value.m[1][1], 1.0);
        assert_eq!(value.m[2][2], 0.0, "the missing row2 keeps the default");
        assert_eq!(value.m[3][3], 1.0);
        assert_eq!(value.m[0][3], 0.0);
    }

    /// The type tables start empty before any interpreter exists.
    #[test]
    fn the_type_tables_start_empty() {
        assert!(type_address(DATA_SECTION_NAME).is_none());
        assert!(type_address(TABLE_NAME).is_none());
        assert!(type_address(STRUCT_NAME).is_none());
    }
}
