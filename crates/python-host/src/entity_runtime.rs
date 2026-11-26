//! The engine-owned half of the script class faces: the def-driven
//! allocation (`PyType_GenericAlloc` straight on the script class) and the
//! `Base` runtime methods the demo scripts drive.
//!
//! The reference never constructs these instances through `tp_new` -- the
//! engine allocates straight on the script class and calls `__init__`
//! itself. Both server faces do it: the base app's `EntityType::
//! newEntityBase` (`server/baseapp/entity_type.cpp:569`) and the cell
//! app's `EntityType::newEntity` (`server/cellapp/entity_type.cpp:142`);
//! the cell entity's `__init__` then fires through `Entity::
//! callScriptInit` (`server/cellapp/entity.cpp:1924`), the base's through
//! `Base::init`'s `callMethod("__init__", ..., /* okIfNull */ true)`
//! (`server/baseapp/base.cpp:794`). Both tolerate the method's absence
//! and print+clear its errors, the `Script::call` contract.
//!
//! The `Base` methods are the base app's own method table
//! (`server/baseapp/base.cpp`): `createInNewSpace` (`:3548`),
//! `registerGlobally` (`:4225`), `destroyCellEntity` (`:4037`) and
//! `destroy` (`:3975`). Their checks are `Base::
//! checkAssociatedCellEntity` (`:3269`'s `prepareForCellCreate` calls it)
//! and the guard chains each body opens with. `registerGlobally` hands
//! the request to `GlobalBases::registerRequest`
//! (`server/baseapp/global_bases.cpp:284`), whose reply fires the script
//! callback with one boolean (`MakeGlobalHandler::onRequestReply`,
//! `global_bases.cpp:233-258`).
//!
//! The runtime behind the methods is a Rust model, not the Mercury
//! plumbing: [`BaseRuntime`] is what a BaseApp implementation supplies,
//! and the demo scripts drive it through the very method faces the
//! reference exposes. Deltas kept visible:
//! - the reference fires the `registerGlobally` callback when the
//!   BaseAppMgr reply arrives (`global_bases.cpp:257`); the port fires it
//!   when the model answers, the Mercury round trip being the
//!   BaseApp's own face.
//! - the reference pickles the registration key and round-trips it
//!   (`global_bases.cpp:294-318`); the port's key is its string form,
//!   the pickle subsystem being the global registry's own face.
//! - the instance engine state lives in a side table keyed by object
//!   address, not C++ members: the port's instances are bare
//!   `PyType_GenericAlloc` bodies, the reference's carry the `Base`/`Entity`
//!   C++ object.
//! - `destroy`'s `deleteFromDB`/`writeToDB` keyword defaults stay the
//!   reference's (`false` / `hasWrittenToDB`); the keyword spell itself is
//!   unreachable while the binding layer's methods are `METH_VARARGS`
//!   (the registered platform gap), and a positional argument hits the
//!   reference's `"Only expecting keyword arguments"` refusal.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

use crate::binding;
use crate::capi;
use crate::pyref::PyRef;

/// Why an engine construction could not proceed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConstructError {
    /// No [`BaseRuntime`] is attached: the engine faces need one.
    NoRuntime,
    /// `PyType_GenericAlloc` returned null; the pending error was printed.
    AllocFailed,
}

/// The runtime model the `Base` methods drive, the BaseApp half of every
/// request the reference sends over Mercury.
pub trait BaseRuntime {
    /// The id the next engine-allocated base carries: `EntityType::create`
    /// receives it from the BaseApp (`server/baseapp/entity_type.cpp:587`).
    fn next_base_id(&mut self) -> u32;
    /// `CellAppMgrInterface::createEntityInNewSpace` (`base.cpp:3601-3620`):
    /// the bool is `shouldPreferThisMachine`.
    fn create_entity_in_new_space(&mut self, base_id: u32, should_prefer_this_machine: bool);
    /// `GlobalBases::registerRequest`'s `registerBaseGlobally` round trip
    /// (`global_bases.cpp:321-325`): the bool is the reply's
    /// `successCode > 0` face (`global_bases.cpp:236`).
    fn register_base_globally(&mut self, base_id: u32, key: &str) -> bool;
    /// `CellAppInterface::destroyEntityArgs` (`base.cpp:4061-4076`).
    fn destroy_cell_entity(&mut self, base_id: u32);
    /// `Base::destroy(deleteFromDB, writeToDB)` (`base.cpp:4028`).
    fn destroy_base(&mut self, base_id: u32, delete_from_db: bool, write_to_db: bool);
}

/// The per-instance engine state of a base, the `Base` C++ members the
/// method bodies read and set (`server/baseapp/base.hpp:174-330`).
#[derive(Clone)]
struct BaseState {
    id: u32,
    is_service_fragment: bool,
    can_be_on_cell: bool,
    is_destroyed: bool,
    is_create_cell_pending: bool,
    is_get_cell_pending: bool,
    is_destroy_cell_pending: bool,
    has_cell_entity: bool,
    has_written_to_db: bool,
}

/// The per-instance engine state of a cell/client entity, the read-only
/// attributes the reference exposes (`server/cellapp/entity.hpp:455-459`).
#[derive(Clone)]
struct EntityState {
    id: u32,
    class_name: String,
    is_destroyed: bool,
}

thread_local! {
    /// The bases this process built. Construct always replaces the entry
    /// for the (possibly recycled) address, so a lookup can only hit the
    /// state the live instance carries.
    static BASE_STATES: RefCell<BTreeMap<usize, BaseState>> =
        const { RefCell::new(BTreeMap::new()) };
    /// The entities this process built, the same rule.
    static ENTITY_STATES: RefCell<BTreeMap<usize, EntityState>> =
        const { RefCell::new(BTreeMap::new()) };
    /// The runtime model the methods drive. The interpreter's faces all
    /// run on this thread, the GIL's own scope.
    static RUNTIME: RefCell<Option<Rc<RefCell<dyn BaseRuntime>>>> =
        RefCell::new(None);
}

/// Attach the runtime model the `Base` methods drive. One at a time per
/// thread: a later attach replaces the earlier, the way a BaseApp rebuild
/// would re-seat the model.
pub fn attach_runtime(runtime: Rc<RefCell<dyn BaseRuntime>>) {
    RUNTIME.with(|slot| *slot.borrow_mut() = Some(runtime));
}

/// Detach the runtime model: the methods refuse until another attach.
pub fn detach_runtime() {
    RUNTIME.with(|slot| *slot.borrow_mut() = None);
}

/// `EntityType::newEntityBase` (`server/baseapp/entity_type.cpp:569`):
/// `PyType_GenericAlloc` straight on the script class -- never `tp_new`,
/// whose inherited refusal the script construction keeps -- then the
/// engine state, then `Base::init`'s `callMethod("__init__", ...,
/// okIfNull)` (`base.cpp:794`). The engine state must exist before
/// `__init__` runs: `Space.py`'s `__init__` calls `createInNewSpace` on
/// the half-built instance.
///
/// # Safety
///
/// The interpreter must be up, this thread must own the GIL, and
/// `script_class` must be a script class standing on the port's
/// `BigWorld.Base`.
pub unsafe fn construct_base(
    script_class: &PyRef,
    can_be_on_cell: bool,
    is_service_fragment: bool,
) -> Result<PyRef, ConstructError> {
    // SAFETY: the thread-local is process-local state under the GIL.
    let id = match with_runtime(|runtime| runtime.next_base_id()) {
        Some(id) => id,
        None => return Err(ConstructError::NoRuntime),
    };
    // SAFETY: `script_class` is a live type object while the interpreter
    // is up; a null allocation leaves the interpreter's error pending.
    let object = unsafe { capi::PyType_GenericAlloc(script_class.as_ptr().cast(), 0) };
    if object.is_null() {
        // The cell face's error handling (`entity_type.cpp:147-151`):
        // print, clear, report failure.
        // SAFETY: a failed allocation left the error pending.
        unsafe {
            capi::PyErr_Print();
            capi::clear_error();
        }
        return Err(ConstructError::AllocFailed);
    }
    BASE_STATES.with(|states| {
        states.borrow_mut().insert(
            object as usize,
            BaseState {
                id,
                is_service_fragment,
                can_be_on_cell,
                is_destroyed: false,
                is_create_cell_pending: false,
                is_get_cell_pending: false,
                is_destroy_cell_pending: false,
                has_cell_entity: false,
                // `Base::init` runs after the entity delegate, whose
                // database face has not written (`base.cpp:780-793`).
                has_written_to_db: false,
            },
        );
    });
    // SAFETY: the instance is a live object this function owns.
    unsafe { call_init(object) };
    // SAFETY: the object came out of `PyType_GenericAlloc` owned.
    Ok(PyRef::from_owned(object).expect("a fresh allocation is a live object"))
}

/// `EntityType::newEntity` (`server/cellapp/entity_type.cpp:139-160`) for
/// the entity apps' `BigWorld.Entity`: the same straight allocation, then
/// `Entity::callScriptInit`'s plain `__init__`
/// (`server/cellapp/entity.cpp:1924` -- non-restore, no nearby-entity
/// argument). The reference exposes the entity's read-only `id`,
/// `className` and `isDestroyed` (`entity.hpp:456-459`); the engine state
/// carries them.
///
/// # Safety
///
/// The interpreter must be up, this thread must own the GIL, and
/// `script_class` must be a script class standing on the port's
/// `BigWorld.Entity`.
pub unsafe fn construct_entity(
    script_class: &PyRef,
    id: u32,
    class_name: &str,
) -> Result<PyRef, ConstructError> {
    // SAFETY: `script_class` is a live type object while the interpreter
    // is up.
    let object = unsafe { capi::PyType_GenericAlloc(script_class.as_ptr().cast(), 0) };
    if object.is_null() {
        // SAFETY: a failed allocation left the error pending.
        unsafe {
            capi::PyErr_Print();
            capi::clear_error();
        }
        return Err(ConstructError::AllocFailed);
    }
    ENTITY_STATES.with(|states| {
        states.borrow_mut().insert(
            object as usize,
            EntityState {
                id,
                class_name: String::from(class_name),
                is_destroyed: false,
            },
        );
    });
    // SAFETY: the instance is a live object this function owns.
    unsafe { call_init(object) };
    // SAFETY: the object came out of `PyType_GenericAlloc` owned.
    Ok(PyRef::from_owned(object).expect("a fresh allocation is a live object"))
}

/// The engine's `__init__` call, shared by both construction faces: a
/// no-arg call whose absence is tolerated (`okIfNull` for the base,
/// `Entity::callback`'s attribute check for the entity) and whose errors
/// print + clear (`ScriptErrorPrint`, the `Script::call` contract).
///
/// # Safety
///
/// `instance` must be a live object while the interpreter is up.
unsafe fn call_init(instance: *mut capi::PyObject) {
    // SAFETY: the instance is live and the name is NUL-terminated.
    let method = unsafe { capi::PyObject_GetAttrString(instance, c"__init__".as_ptr()) };
    if method.is_null() {
        // Absent `__init__`: the tolerated face.
        // SAFETY: the failed lookup left the error pending.
        unsafe { capi::clear_error() };
        return;
    }
    // SAFETY: the method object is live and owned.
    let result = unsafe { capi::PyObject_CallNoArgs(method) };
    // SAFETY: the method handle is owned here.
    unsafe { capi::dec_ref(method) };
    if result.is_null() {
        // SAFETY: the failed call left a pending exception, which the
        // print consumes.
        unsafe {
            capi::PyErr_PrintEx(0);
            capi::clear_error();
        }
    } else {
        // SAFETY: the result came out of the call owned.
        unsafe { capi::dec_ref(result) };
    }
}

/// Fire one no-arg engine callback on the instance: `Entity::callback` /
/// `Script::call` semantics -- absent tolerated, errors print + clear.
///
/// # Safety
///
/// `instance` must be a live object while the interpreter is up, and
/// `name` NUL-terminated.
unsafe fn call_callback(instance: *mut capi::PyObject, name: &std::ffi::CStr) {
    // SAFETY: the instance is live and the name is NUL-terminated.
    let method = unsafe { capi::PyObject_GetAttrString(instance, name.as_ptr()) };
    if method.is_null() {
        // SAFETY: the failed lookup left the error pending.
        unsafe { capi::clear_error() };
        return;
    }
    // SAFETY: the method object is live and owned; the empty argument
    // tuple is the no-arg callback face.
    unsafe {
        let args = capi::PyTuple_New(0);
        let result = capi::PyObject_CallObject(method, args);
        if !args.is_null() {
            capi::dec_ref(args);
        }
        capi::dec_ref(method);
        if result.is_null() {
            capi::PyErr_PrintEx(0);
            capi::clear_error();
        } else {
            capi::dec_ref(result);
        }
    }
}

/// The created cell entity contacted its base: `isGetCellPending_` clears
/// and `hasCellEntity()` turns true (`base.cpp`'s cell-registration
/// arrival). The base app implementation calls this when the reply lands.
///
/// # Safety
///
/// `base` must be a live instance `construct_base` built while the
/// interpreter is up.
pub unsafe fn cell_entity_registered(base: *mut capi::PyObject) {
    BASE_STATES.with(|states| {
        if let Some(state) = states.borrow_mut().get_mut(&(base as usize)) {
            state.is_create_cell_pending = false;
            state.is_get_cell_pending = false;
            state.has_cell_entity = true;
        }
    });
}

/// The cell entity was lost (its cell retired or the space tore down):
/// the cell faces clear and the script's `onLoseCell` fires -- `Space.py`
/// drives `self.destroy()` from exactly this callback.
///
/// # Safety
///
/// `base` must be a live instance `construct_base` built while the
/// interpreter is up.
pub unsafe fn cell_entity_lost(base: *mut capi::PyObject) {
    BASE_STATES.with(|states| {
        if let Some(state) = states.borrow_mut().get_mut(&(base as usize)) {
            state.is_create_cell_pending = false;
            state.is_get_cell_pending = false;
            state.is_destroy_cell_pending = false;
            state.has_cell_entity = false;
        }
    });
    // SAFETY: the instance is live while the interpreter is up.
    unsafe { call_callback(base, c"onLoseCell") };
}

/// Borrow the runtime for one model call. `None` when none is attached --
/// the caller turns that into the interpreter's error. The borrow lives
/// exactly across the model call: the model never re-enters the
/// interpreter, so the guard can never be re-borrowed.
fn with_runtime<R>(body: impl FnOnce(&mut dyn BaseRuntime) -> R) -> Option<R> {
    RUNTIME.with(|slot| {
        let mut slot = slot.borrow_mut();
        let runtime = slot.as_mut()?;
        let mut model = (**runtime).borrow_mut();
        Some(body(&mut *model))
    })
}

/// Whether a runtime model is attached. The method handlers check this up
/// front -- the guard chains need a model to be meaningful -- and then take
/// each model call through [`with_runtime`].
fn runtime_attached() -> bool {
    RUNTIME.with(|slot| slot.borrow().is_some())
}

/// Read the engine state of a base instance.
fn base_state(instance: *mut capi::PyObject) -> Option<BaseState> {
    BASE_STATES.with(|states| states.borrow().get(&(instance as usize)).cloned())
}

/// Mutate the engine state of a base instance in place.
fn update_base_state(instance: *mut capi::PyObject, body: impl FnOnce(&mut BaseState)) {
    BASE_STATES.with(|states| {
        if let Some(state) = states.borrow_mut().get_mut(&(instance as usize)) {
            body(state);
        }
    });
}

/// Read the engine state of an entity instance.
fn entity_state(instance: *mut capi::PyObject) -> Option<EntityState> {
    ENTITY_STATES.with(|states| states.borrow().get(&(instance as usize)).cloned())
}

/// Raise a formatted `ValueError`, the reference's `PyErr_Format` faces.
fn value_error(message: String) {
    // SAFETY: the message is a fresh unicode and the exception constant is
    // the global `ValueError`.
    unsafe {
        let rendered = capi::unicode_from_bytes(message.as_bytes());
        capi::PyErr_SetObject(capi::PyExc_ValueError, rendered);
        capi::dec_ref(rendered);
    }
}

/// Raise a formatted `AttributeError`, the reference's `PyErr_Format`.
fn attribute_error(message: String) {
    // SAFETY: the message is a fresh unicode and the exception constant is
    // the global `AttributeError`.
    unsafe {
        let rendered = capi::unicode_from_bytes(message.as_bytes());
        capi::PyErr_SetObject(capi::PyExc_AttributeError, rendered);
        capi::dec_ref(rendered);
    }
}

/// Raise a `TypeError` with a plain message, the parse/guard faces.
fn type_error(message: &str) {
    // SAFETY: the message is a fresh unicode and the exception constant is
    // the global `TypeError`.
    unsafe {
        let rendered = capi::unicode_from_bytes(message.as_bytes());
        capi::PyErr_SetObject(capi::PyExc_TypeError, rendered);
        capi::dec_ref(rendered);
    }
}

/// Raise the port's internal-refusal face for a caller-contract miss (no
/// runtime attached, engine state absent). The reference cannot reach
/// either -- its methods live on a BaseApp that owns its bases.
fn system_error(message: &str) {
    // SAFETY: the message is a fresh unicode and the exception constant is
    // the global `SystemError`.
    unsafe {
        let rendered = capi::unicode_from_bytes(message.as_bytes());
        capi::PyErr_SetObject(capi::PyExc_SystemError, rendered);
        capi::dec_ref(rendered);
    }
}

/// Return `None` to the interpreter, a fresh reference.
fn return_none() -> *mut capi::PyObject {
    // SAFETY: the singleton is immortal; the caller's return owns the
    // reference.
    unsafe {
        capi::inc_ref(capi::py_none());
        capi::py_none()
    }
}

/// `Base.createInNewSpace` (`base.cpp:3548`): the
/// `prepareForCellCreate` guard chain (`:3269` -- the
/// `checkAssociatedCellEntity` arms, then the cell-script check), then
/// both pendings set and the model call. The reference's optional
/// `shouldPreferThisMachine` keyword crosses as an optional positional
/// (the binding layer's registered `METH_VARARGS` gap); the parse failure
/// wording beyond that stays the interpreter's.
///
/// # Safety
///
/// CPython calls this with a borrowed self and args tuple.
unsafe extern "C" fn base_create_in_new_space(
    self_: *mut capi::PyObject,
    args: *mut capi::PyObject,
) -> *mut capi::PyObject {
    if !runtime_attached() {
        system_error("no base runtime attached");
        return std::ptr::null_mut();
    }
    let Some(state) = base_state(self_) else {
        system_error("engine state missing for this instance");
        return std::ptr::null_mut();
    };
    // The `checkAssociatedCellEntity(false)` chain (`base.cpp:3230-3267`)
    // with this method's error prefix.
    if state.is_destroyed {
        value_error(format!(
            "Base.createEntityInNewSpace: Base {} is destroyed",
            state.id
        ));
        return std::ptr::null_mut();
    }
    if state.is_create_cell_pending {
        value_error(format!(
            "Base.createEntityInNewSpace: Base {} is in the process of \
             creating an associated cell entity.",
            state.id
        ));
        return std::ptr::null_mut();
    }
    if state.is_get_cell_pending {
        value_error(format!(
            "Base.createEntityInNewSpace: Base {} is waiting for contact \
             from its created associated cell entity.\n",
            state.id
        ));
        return std::ptr::null_mut();
    }
    if state.has_cell_entity {
        value_error(format!(
            "Base.createEntityInNewSpace: Base {} already has an \
             associated cell entity.",
            state.id
        ));
        return std::ptr::null_mut();
    }
    if !state.can_be_on_cell {
        attribute_error(format!(
            "Base.createEntityInNewSpace: Base {} has no cell script.",
            state.id
        ));
        return std::ptr::null_mut();
    }
    // `|O` -- zero or one optional `shouldPreferThisMachine`.
    // SAFETY: `args` is the tuple CPython handed the call.
    let size = unsafe { capi::PyTuple_Size(args) };
    if size > 1 {
        type_error("Base.createEntityInNewSpace() takes at most 1 argument");
        return std::ptr::null_mut();
    }
    let mut should_prefer_this_machine = false;
    if size == 1 {
        // SAFETY: the index is inside the checked size.
        let flag = unsafe { capi::PyTuple_GetItem(args, 0) };
        // SAFETY: the flag object is a borrowed live object.
        match unsafe { capi::PyObject_IsTrue(flag) } {
            1 => should_prefer_this_machine = true,
            0 => {}
            // The conversion failed: its error is pending.
            _ => return std::ptr::null_mut(),
        }
    }
    // Both pendings set before the send (`base.cpp:3288-3289`).
    update_base_state(self_, |state| {
        state.is_create_cell_pending = true;
        state.is_get_cell_pending = true;
    });
    // The model call runs under the GIL and never re-enters.
    with_runtime(|runtime| {
        runtime.create_entity_in_new_space(state.id, should_prefer_this_machine);
    });
    return_none()
}

/// `Base.registerGlobally` (`base.cpp:4225`): the service refusal, the
/// callable check, the key's string form (the port's pickle-face delta),
/// the model request, then the callback with one boolean
/// (`MakeGlobalHandler::onRequestReply`, `global_bases.cpp:233-258`).
///
/// # Safety
///
/// CPython calls this with a borrowed self and args tuple.
unsafe extern "C" fn base_register_globally(
    self_: *mut capi::PyObject,
    args: *mut capi::PyObject,
) -> *mut capi::PyObject {
    if !runtime_attached() {
        system_error("no base runtime attached");
        return std::ptr::null_mut();
    }
    let Some(state) = base_state(self_) else {
        system_error("engine state missing for this instance");
        return std::ptr::null_mut();
    };
    if state.is_service_fragment {
        type_error("Services cannot be registered globally");
        return std::ptr::null_mut();
    }
    // SAFETY: `args` is the tuple CPython handed the call.
    let size = unsafe { capi::PyTuple_Size(args) };
    if size != 2 {
        type_error("registerGlobally() takes exactly 2 arguments");
        return std::ptr::null_mut();
    }
    // SAFETY: the indices are inside the checked size.
    let (key, callback) = unsafe {
        (
            capi::PyTuple_GetItem(args, 0),
            capi::PyTuple_GetItem(args, 1),
        )
    };
    // SAFETY: the callback is a borrowed live object.
    if unsafe { capi::PyCallable_Check(callback) } == 0 {
        type_error("The second argument must be callable");
        return std::ptr::null_mut();
    }
    // The key's string form (`PyObject_Str` + UTF-8, `base.cpp:4248-4273`
    // best-effort conversion); a failure maps to the reference's
    // empty-pickle refusal. The model call and the callback fire with no
    // borrow held: the callback is script code and may drive the Base
    // methods again.
    // SAFETY: the key is a borrowed live object.
    let rendered = unsafe { capi::PyObject_Str(key) };
    if rendered.is_null() {
        // SAFETY: the failed conversion left the error pending.
        unsafe { capi::clear_error() };
        type_error("Unable to pickle key argument");
        return std::ptr::null_mut();
    }
    // SAFETY: the rendered object is owned here.
    let key_text = unsafe { capi::to_string(rendered) };
    // SAFETY: the rendered object is owned here.
    unsafe { capi::dec_ref(rendered) };
    let succeeded = with_runtime(|runtime| runtime.register_base_globally(state.id, &key_text))
        .unwrap_or(false);
    // `Py_True`/`Py_False` by the reply's success code
    // (`global_bases.cpp:236-244`).
    let flag = if succeeded {
        capi::py_true()
    } else {
        capi::py_false()
    };
    // SAFETY: the callback is a borrowed live object; the fresh tuple
    // holds its own reference to the flag.
    unsafe {
        let tuple = capi::PyTuple_Pack(1, flag);
        let result = if tuple.is_null() {
            std::ptr::null_mut()
        } else {
            capi::PyObject_CallObject(callback, tuple)
        };
        if !tuple.is_null() {
            capi::dec_ref(tuple);
        }
        if result.is_null() {
            capi::PyErr_PrintEx(0);
            capi::clear_error();
        } else {
            capi::dec_ref(result);
        }
    }
    return_none()
}

/// `Base.destroyCellEntity` (`base.cpp:4037`): the pending-get
/// short-circuit, then the `checkAssociatedCellEntity(true)` chain, the
/// no-arguments refusal, the resend guard, then the model call.
///
/// # Safety
///
/// CPython calls this with a borrowed self and args tuple.
unsafe extern "C" fn base_destroy_cell_entity(
    self_: *mut capi::PyObject,
    args: *mut capi::PyObject,
) -> *mut capi::PyObject {
    if !runtime_attached() {
        system_error("no base runtime attached");
        return std::ptr::null_mut();
    }
    let Some(state) = base_state(self_) else {
        system_error("engine state missing for this instance");
        return std::ptr::null_mut();
    };
    // `!isGetCellPending() && !checkAssociatedCellEntity(true, ...)`
    // (`base.cpp:4041-4045`): a pending get-cell counts as one on its
    // way, and skips the check entirely.
    if !state.is_get_cell_pending {
        if state.is_destroyed {
            value_error(format!(
                "Base.destroyCellEntity: Base {} is destroyed",
                state.id
            ));
            return std::ptr::null_mut();
        }
        if state.is_create_cell_pending {
            value_error(format!(
                "Base.destroyCellEntity: Base {} is in the process of \
                 creating an associated cell entity.",
                state.id
            ));
            return std::ptr::null_mut();
        }
        if !state.has_cell_entity {
            value_error(format!(
                "Base.destroyCellEntity: Base {} does not have an \
                 associated cell entity.",
                state.id
            ));
            return std::ptr::null_mut();
        }
    }
    // SAFETY: `args` is the tuple CPython handed the call.
    if unsafe { capi::PyTuple_Size(args) } > 0 {
        type_error("Base.destroyCellEntity() accepts no arguments");
        return std::ptr::null_mut();
    }
    // The resend guard (`base.cpp:4056-4076`).
    let already_pending = base_state(self_).is_some_and(|state| state.is_destroy_cell_pending);
    if !already_pending {
        // The model call runs under the GIL and never re-enters.
        with_runtime(|runtime| runtime.destroy_cell_entity(state.id));
        update_base_state(self_, |state| state.is_destroy_cell_pending = true);
    }
    return_none()
}

/// `Base.destroy` (`base.cpp:3975`): the positional refusal (the
/// reference wants keyword-only extras), the still-has-cell guard, the
/// double-destroy guard, then the model call with the reference's
/// defaults -- `deleteFromDB` false, `writeToDB` = `hasWrittenToDB()` --
/// while the keyword spell itself stays behind the binding layer's
/// `METH_VARARGS` gap.
///
/// # Safety
///
/// CPython calls this with a borrowed self and args tuple.
unsafe extern "C" fn base_destroy(
    self_: *mut capi::PyObject,
    args: *mut capi::PyObject,
) -> *mut capi::PyObject {
    if !runtime_attached() {
        system_error("no base runtime attached");
        return std::ptr::null_mut();
    }
    let Some(state) = base_state(self_) else {
        system_error("engine state missing for this instance");
        return std::ptr::null_mut();
    };
    // SAFETY: `args` is the tuple CPython handed the call.
    if unsafe { capi::PyTuple_Size(args) } != 0 {
        type_error("Only expecting keyword arguments");
        return std::ptr::null_mut();
    }
    if state.has_cell_entity || state.is_get_cell_pending {
        value_error(String::from(
            "Still has cell entity. Use Base.destroyCellEntity",
        ));
        return std::ptr::null_mut();
    }
    if state.is_destroyed {
        value_error(String::from("Base entity already destroyed"));
        return std::ptr::null_mut();
    }
    let delete_from_db = false;
    let write_to_db = state.has_written_to_db;
    // The model call runs under the GIL and never re-enters.
    with_runtime(|runtime| runtime.destroy_base(state.id, delete_from_db, write_to_db));
    update_base_state(self_, |state| state.is_destroyed = true);
    return_none()
}

/// The `Base` read-only `id` (`base.hpp:241`).
///
/// # Safety
///
/// CPython calls this with a borrowed instance.
unsafe extern "C" fn base_id_getter(
    self_: *mut capi::PyObject,
    _closure: *mut std::os::raw::c_void,
) -> *mut capi::PyObject {
    let Some(state) = base_state(self_) else {
        system_error("engine state missing for this instance");
        return std::ptr::null_mut();
    };
    // SAFETY: the interpreter is up; the id is a plain integer.
    unsafe { capi::PyLong_FromLong(i64::from(state.id) as _) }
}

/// The `Base` read-only `isDestroyed` (`base.hpp:242`).
///
/// # Safety
///
/// CPython calls this with a borrowed instance.
unsafe extern "C" fn base_is_destroyed_getter(
    self_: *mut capi::PyObject,
    _closure: *mut std::os::raw::c_void,
) -> *mut capi::PyObject {
    let Some(state) = base_state(self_) else {
        system_error("engine state missing for this instance");
        return std::ptr::null_mut();
    };
    // SAFETY: the interpreter is up.
    unsafe { capi::PyBool_FromLong(i64::from(state.is_destroyed) as _) }
}

/// The entity's read-only `id` (`entity.hpp:457`).
///
/// # Safety
///
/// CPython calls this with a borrowed instance.
unsafe extern "C" fn entity_id_getter(
    self_: *mut capi::PyObject,
    _closure: *mut std::os::raw::c_void,
) -> *mut capi::PyObject {
    let Some(state) = entity_state(self_) else {
        system_error("engine state missing for this instance");
        return std::ptr::null_mut();
    };
    // SAFETY: the interpreter is up; the id is a plain integer.
    unsafe { capi::PyLong_FromLong(i64::from(state.id) as _) }
}

/// The entity's read-only `className` (`entity.hpp:456`).
///
/// # Safety
///
/// CPython calls this with a borrowed instance.
unsafe extern "C" fn entity_class_name_getter(
    self_: *mut capi::PyObject,
    _closure: *mut std::os::raw::c_void,
) -> *mut capi::PyObject {
    let Some(state) = entity_state(self_) else {
        system_error("engine state missing for this instance");
        return std::ptr::null_mut();
    };
    // SAFETY: the interpreter is up.
    unsafe { capi::unicode_from_bytes(state.class_name.as_bytes()) }
}

/// The entity's read-only `isDestroyed` (`entity.hpp:459`).
///
/// # Safety
///
/// CPython calls this with a borrowed instance.
unsafe extern "C" fn entity_is_destroyed_getter(
    self_: *mut capi::PyObject,
    _closure: *mut std::os::raw::c_void,
) -> *mut capi::PyObject {
    let Some(state) = entity_state(self_) else {
        system_error("engine state missing for this instance");
        return std::ptr::null_mut();
    };
    // SAFETY: the interpreter is up.
    unsafe { capi::PyBool_FromLong(i64::from(state.is_destroyed) as _) }
}

/// The `BigWorld.Base` shape: subclassable (the import face's own rule)
/// plus the base app's method table and read-only attributes. The
/// reference's method table is `base.cpp`'s `PY_METHOD` block; the port
/// carries the four faces the demo scripts and the guard chains use.
pub fn shape_base(spec: binding::TypeSpec) -> binding::TypeSpec {
    spec.subclassable()
        .method("createInNewSpace", base_create_in_new_space)
        .method("registerGlobally", base_register_globally)
        .method("destroyCellEntity", base_destroy_cell_entity)
        .method("destroy", base_destroy)
        .attribute("id", base_id_getter, None)
        .attribute("isDestroyed", base_is_destroyed_getter, None)
}

/// The `BigWorld.Entity` shape: subclassable plus the cell entity's
/// read-only attribute trio (`entity.hpp:456-459`).
pub fn shape_entity(spec: binding::TypeSpec) -> binding::TypeSpec {
    spec.subclassable()
        .attribute("id", entity_id_getter, None)
        .attribute("className", entity_class_name_getter, None)
        .attribute("isDestroyed", entity_is_destroyed_getter, None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::import_paths;
    use crate::interpreter::{InitConfig, Interpreter};
    use std::path::PathBuf;
    use std::process::Command;

    /// Every scenario, each in its own process: the interpreter refuses a
    /// second init in one process, the same rule every harness follows.
    const SCENARIOS: &[&str] = &[
        "entity_runtime::tests::scenario_the_default_space_bootstrap_runs",
        "entity_runtime::tests::scenario_a_failed_global_registration_drives_the_failure_arm",
        "entity_runtime::tests::scenario_the_base_guards_hold_the_reference_messages",
        "entity_runtime::tests::scenario_the_cell_guard_entity_constructs",
        "entity_runtime::tests::scenario_the_client_guard_entity_constructs",
        "entity_runtime::tests::scenario_the_runtime_and_destroy_faces_hold",
        "entity_runtime::tests::scenario_constructing_without_a_runtime_refuses",
    ];

    #[test]
    fn the_scenarios_run() {
        for scenario in SCENARIOS {
            run_scenario(scenario);
        }
    }

    /// Runs `scenario` in a fresh process of this test binary and requires
    /// it to exit normally.
    fn run_scenario(scenario: &str) {
        let harness = std::env::current_exe().expect("the test binary path");
        let output = Command::new(&harness)
            .args(["--exact", scenario, "--ignored", "--test-threads=1"])
            .output()
            .expect("spawn the scenario child");
        assert!(
            output.status.success(),
            "scenario {scenario} did not exit normally: {}\n--- stdout ---\n{}\n--- stderr ---\n{}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
    }

    /// The one call record the model ledger captures.
    #[derive(Clone, Debug, PartialEq)]
    enum Call {
        CreateInNewSpace(u32, bool),
        RegisterGlobally(u32, String, bool),
        DestroyCellEntity(u32),
        Destroy(u32, bool, bool),
    }

    /// The model ledger: every request the scripts drive, in order. The
    /// registration outcome is configurable, the space-create reply face
    /// is driven by hand through [`cell_entity_registered`]/[`cell_entity_lost`].
    #[derive(Default)]
    struct Ledger {
        calls: Vec<Call>,
        next_id: u32,
        register_succeeds: bool,
    }

    impl Ledger {
        fn new(register_succeeds: bool) -> Self {
            Self {
                calls: Vec::new(),
                next_id: 1,
                register_succeeds,
            }
        }
    }

    impl BaseRuntime for Ledger {
        fn next_base_id(&mut self) -> u32 {
            let id = self.next_id;
            self.next_id += 1;
            id
        }
        fn create_entity_in_new_space(&mut self, base_id: u32, should_prefer_this_machine: bool) {
            self.calls
                .push(Call::CreateInNewSpace(base_id, should_prefer_this_machine));
        }
        fn register_base_globally(&mut self, base_id: u32, key: &str) -> bool {
            let succeeded = self.register_succeeds;
            self.calls.push(Call::RegisterGlobally(
                base_id,
                String::from(key),
                succeeded,
            ));
            succeeded
        }
        fn destroy_cell_entity(&mut self, base_id: u32) {
            self.calls.push(Call::DestroyCellEntity(base_id));
        }
        fn destroy_base(&mut self, base_id: u32, delete_from_db: bool, write_to_db: bool) {
            self.calls
                .push(Call::Destroy(base_id, delete_from_db, write_to_db));
        }
    }

    /// `Interpreter::init` around one body, then `Script::fini`. The path
    /// list carries the role's demo scripts from the generated fantasydemo
    /// fixture tree, so `import Space` inside the body sees the role's
    /// registered surface, unchanged text.
    fn interpreter_scenario(component: &str, body: impl FnOnce()) {
        let root = temp_resource_root();
        let role = demo_scripts_dir(component);
        let mut paths = import_paths::ImportPaths::default();
        paths.add_res_path(&[&root.to_string_lossy()], import_paths::COMMON_PATH);
        paths.add_non_res_path(&role.to_string_lossy());
        let interpreter = Interpreter::init(&InitConfig {
            component: String::from(component),
            import_paths: paths,
            ..InitConfig::default()
        })
        .expect("Script::init");
        body();
        interpreter.fini();
    }

    /// A throw-away resource root with the one module the interpreter's
    /// startup always imports: `bw_site`.
    fn temp_resource_root() -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "boundless-python-host-entity-runtime-{}",
            std::process::id()
        ));
        std::fs::remove_dir_all(&root).ok();
        std::fs::create_dir_all(root.join("scripts/common/Lib")).expect("build the script tree");
        std::fs::write(
            root.join("scripts/common/bw_site.py"),
            "import sys\nsys.bw_site_imported = True\n",
        )
        .expect("write bw_site");
        root
    }

    /// The generated fantasydemo fixture scripts of one role, in the repo.
    fn demo_scripts_dir(component: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../docs/review/fixtures/fantasydemo_def/res/scripts")
            .join(component)
    }

    /// Runs `source` and reads the `__probe` it assigns back as a string.
    fn probe(source: &str) -> String {
        // SAFETY: only called from a scenario body, while the interpreter
        // is up on the thread that owns the GIL.
        unsafe {
            let Ok(encoded) = std::ffi::CString::new(source) else {
                panic!("probe source has no embedded NUL");
            };
            assert!(
                capi::PyRun_SimpleString(encoded.as_ptr()) == 0,
                "script must run: {source}"
            );
            let main = capi::PyImport_AddModule(c"__main__".as_ptr());
            let dictionary = capi::PyModule_GetDict(main);
            let value = capi::PyDict_GetItemString(dictionary, c"__probe".as_ptr());
            let text = capi::PyObject_Str(value);
            let rendered = capi::to_string(text);
            capi::dec_ref(text);
            rendered
        }
    }

    /// Runs `source` and reads the `TypeError`/`ValueError`/
    /// `AttributeError`/`SystemError` (whichever the `except` arm caught)
    /// message back as a string. `SystemError` is the port's
    /// internal-refusal face, so it joins the caught set.
    fn probe_error(source: &str) -> String {
        probe(&format!(
            "try:\n\
             \x20{source}\n\
             except (TypeError, ValueError, AttributeError, SystemError) as error:\n\
             \x20__probe = str(error)\n\
             else:\n\
             \x20__probe = 'no error'"
        ))
    }

    /// Imports one fixture script module and hands its same-named class
    /// back as an owned reference.
    fn fixture_class(module: &str) -> PyRef {
        // SAFETY: only called from a scenario body, while the interpreter
        // is up on the thread that owns the GIL.
        unsafe {
            let imported =
                capi::PyImport_ImportModule(std::ffi::CString::new(module).unwrap().as_ptr());
            assert!(!imported.is_null(), "import {module}");
            let class = capi::PyObject_GetAttrString(
                imported,
                std::ffi::CString::new(module).unwrap().as_ptr(),
            );
            capi::dec_ref(imported);
            assert!(!class.is_null(), "{module}.{module}");
            PyRef::from_owned(class).expect("a fresh attribute reference is live")
        }
    }

    /// Reads a name the probe script left behind in `__main__` -- a class
    /// the scenario defined, the same channel `probe` reports values through.
    fn main_value(name: &str) -> PyRef {
        // SAFETY: only called from a scenario body, while the interpreter
        // is up on the thread that owns the GIL.
        unsafe {
            let main = capi::PyImport_AddModule(c"__main__".as_ptr());
            let dictionary = capi::PyModule_GetDict(main);
            let value = capi::PyDict_GetItemString(
                dictionary,
                std::ffi::CString::new(name).unwrap().as_ptr(),
            );
            assert!(!value.is_null(), "{name} is not defined in __main__");
            PyRef::borrowed(value)
        }
    }

    /// Stashes a Rust-built instance into `__main__` under `name`, so the
    /// probe scripts can inspect it.
    fn stash(name: &str, instance: &PyRef) {
        // SAFETY: the interpreter is up; the instance is live and the name
        // is NUL-terminated.
        unsafe {
            let main = capi::PyImport_AddModule(c"__main__".as_ptr());
            assert!(
                capi::PyObject_SetAttrString(
                    main,
                    std::ffi::CString::new(name).unwrap().as_ptr(),
                    instance.as_ptr(),
                ) == 0,
                "stash {name}"
            );
        }
    }

    /// The DefaultSpace bootstrap, run exactly as the reference base app
    /// runs it: the fixture `Space.py` class is engine-constructed, its
    /// `__init__` drives `createInNewSpace()` then
    /// `registerGlobally("DefaultSpace", self.onRegistered)`, the model
    /// answers success, and the `onRegistered(True)` arm does nothing --
    /// the script-visible behaviour is the ledger's two requests in order
    /// plus the base's engine attributes.
    #[test]
    #[ignore = "runs in its own process: see SCENARIOS"]
    fn scenario_the_default_space_bootstrap_runs() {
        interpreter_scenario("base", || {
            let ledger = Rc::new(RefCell::new(Ledger::new(true)));
            attach_runtime(ledger.clone());
            let space_class = fixture_class("Space");
            // SAFETY: the interpreter is up and the class stands on the
            // port's `BigWorld.Base`.
            let space = unsafe { construct_base(&space_class, true, false) }
                .expect("the engine builds the DefaultSpace bootstrap");
            stash("space", &space);
            assert_eq!(
                probe(
                    "import BigWorld\n\
                     __probe = repr([\n\
                     \x20 space.id,\n\
                     \x20 space.isDestroyed,\n\
                     \x20 type(space).__name__,\n\
                     \x20 isinstance(space, BigWorld.Base),\n\
                     ])"
                ),
                "[1, False, 'Space', True]",
                "the engine-built base carries the reference's read-only \
                 attributes"
            );
            // The `onRegistered(True)` arm does nothing -- no exception
            // surfaced from the callback (a print would have landed on
            // stderr and the probe would still pass, so assert the ledger
            // saw exactly the two requests and nothing after).
            assert_eq!(
                ledger.borrow().calls,
                vec![
                    Call::CreateInNewSpace(1, false),
                    Call::RegisterGlobally(1, String::from("DefaultSpace"), true),
                ],
                "the bootstrap drove exactly the two requests in order"
            );
            detach_runtime();
        });
    }

    /// A failed global registration drives the script's failure arm:
    /// `onRegistered(False)` calls `destroyCellEntity()`, which -- with
    /// the space create still pending (`isGetCellPending`) -- skips the
    /// has-cell check and sends the destroy. That ordering is the
    /// reference's own behaviour for the bootstrap script.
    #[test]
    #[ignore = "runs in its own process: see SCENARIOS"]
    fn scenario_a_failed_global_registration_drives_the_failure_arm() {
        interpreter_scenario("base", || {
            let ledger = Rc::new(RefCell::new(Ledger::new(false)));
            attach_runtime(ledger.clone());
            let space_class = fixture_class("Space");
            // SAFETY: the interpreter is up and the class stands on the
            // port's `BigWorld.Base`.
            unsafe { construct_base(&space_class, true, false) }
                .expect("the engine builds the base");
            assert_eq!(
                ledger.borrow().calls,
                vec![
                    Call::CreateInNewSpace(1, false),
                    Call::RegisterGlobally(1, String::from("DefaultSpace"), false),
                    // The failure arm's `destroyCellEntity()`: no
                    // `does not have an associated cell entity` error --
                    // the pending get-cell skips the check.
                    Call::DestroyCellEntity(1),
                ],
                "the failure arm destroyed the pending cell entity"
            );
            detach_runtime();
        });
    }

    /// The guard chains hold the reference's messages, base by base. A
    /// fresh engine-built base walks `destroyCellEntity` (no cell), the
    /// double `createInNewSpace`, the non-callable callback, the
    /// still-has-cell destroy, the completed create, the destroy during
    /// and after a cell entity, the double destroy, and the destroyed
    /// create -- every message the reference bodies format.
    #[test]
    #[ignore = "runs in its own process: see SCENARIOS"]
    fn scenario_the_base_guards_hold_the_reference_messages() {
        interpreter_scenario("base", || {
            let ledger = Rc::new(RefCell::new(Ledger::new(true)));
            attach_runtime(ledger.clone());
            // A plain script class over `BigWorld.Base`, the way the demo
            // scripts define theirs.
            probe(
                "import BigWorld\n\
                 class Widget(BigWorld.Base):\n\
                 \x20 pass\n\
                 class NoCell(BigWorld.Base):\n\
                 \x20 pass\n\
                 __probe = 'defined'",
            );
            // SAFETY: the interpreter is up and the classes stand on the
            // port's `BigWorld.Base`.
            let widget_class = main_value("Widget");
            let widget = unsafe { construct_base(&widget_class, true, false) }
                .expect("the engine builds Widget");
            stash("widget", &widget);
            assert_eq!(
                probe_error("widget.destroyCellEntity()"),
                "Base.destroyCellEntity: Base 1 does not have an associated \
                 cell entity."
            );
            assert_eq!(
                probe_error("widget.createInNewSpace(True, False)"),
                "Base.createEntityInNewSpace() takes at most 1 argument"
            );
            probe("widget.createInNewSpace()");
            assert_eq!(
                probe_error("widget.createInNewSpace()"),
                "Base.createEntityInNewSpace: Base 1 is in the process of \
                 creating an associated cell entity."
            );
            assert_eq!(
                probe_error("widget.registerGlobally('k', 42)"),
                "The second argument must be callable"
            );
            assert_eq!(
                probe_error("widget.destroy()"),
                "Still has cell entity. Use Base.destroyCellEntity"
            );
            // The space-create reply lands: the pendings clear, the cell
            // entity is associated.
            // SAFETY: the instance is a live engine-built base.
            unsafe { cell_entity_registered(widget.as_ptr()) };
            assert_eq!(
                probe_error("widget.destroy()"),
                "Still has cell entity. Use Base.destroyCellEntity"
            );
            // The state check precedes the argument check
            // (`base.cpp:4041-4053`): with a cell entity associated, the
            // argument refusal is the one that fires.
            assert_eq!(
                probe_error("widget.destroyCellEntity('x')"),
                "Base.destroyCellEntity() accepts no arguments"
            );
            probe("widget.destroyCellEntity()");
            assert_eq!(
                probe_error("widget.destroy()"),
                "Still has cell entity. Use Base.destroyCellEntity"
            );
            // The cell entity is lost: `onLoseCell` fires (Widget has
            // none -- tolerated), the cell faces clear.
            // SAFETY: the instance is a live engine-built base.
            unsafe { cell_entity_lost(widget.as_ptr()) };
            probe("widget.destroy()");
            assert_eq!(probe("__probe = repr(widget.isDestroyed)"), "True");
            assert_eq!(
                probe_error("widget.destroy()"),
                "Base entity already destroyed"
            );
            assert_eq!(
                probe_error("widget.createInNewSpace()"),
                "Base.createEntityInNewSpace: Base 1 is destroyed"
            );
            // On a destroyed base the state check still precedes the
            // argument check (`base.cpp:4041-4045` first).
            assert_eq!(
                probe_error("widget.destroyCellEntity('x')"),
                "Base.destroyCellEntity: Base 1 is destroyed"
            );
            assert_eq!(
                probe_error("widget.destroy(1)"),
                "Only expecting keyword arguments"
            );
            // The model saw the two sends and the destroy, in order.
            assert_eq!(
                ledger.borrow().calls,
                vec![
                    Call::CreateInNewSpace(1, false),
                    Call::DestroyCellEntity(1),
                    Call::Destroy(1, false, false),
                ]
            );
            // A base whose def has no cell script: the create refuses
            // before any request.
            let no_cell_class = main_value("NoCell");
            let no_cell = unsafe { construct_base(&no_cell_class, false, false) }
                .expect("the engine builds NoCell");
            stash("no_cell", &no_cell);
            assert_eq!(
                probe_error("no_cell.createInNewSpace()"),
                "Base.createEntityInNewSpace: Base 2 has no cell script."
            );
            assert!(ledger.borrow().calls.len() == 3, "no request left");
            detach_runtime();
        });
    }

    /// The cell Guard: the fixture `scripts/cell/Guard.py` class
    /// engine-constructed -- the allocation straight on the script class,
    /// `__init__` driven, and the reference's read-only attribute trio on
    /// the instance.
    #[test]
    #[ignore = "runs in its own process: see SCENARIOS"]
    fn scenario_the_cell_guard_entity_constructs() {
        interpreter_scenario("cell", || {
            let guard_class = fixture_class("Guard");
            // SAFETY: the interpreter is up and the class stands on the
            // port's `BigWorld.Entity`.
            let guard = unsafe { construct_entity(&guard_class, 7, "Guard") }
                .expect("the engine builds the cell Guard");
            stash("guard", &guard);
            assert_eq!(
                probe(
                    "import BigWorld\n\
                     __probe = repr([\n\
                     \x20 guard.id,\n\
                     \x20 guard.className,\n\
                     \x20 guard.isDestroyed,\n\
                     \x20 type(guard).__name__,\n\
                     \x20 isinstance(guard, BigWorld.Entity),\n\
                     ])"
                ),
                "[7, 'Guard', False, 'Guard', True]",
                "the engine-built entity carries the reference's read-only \
                 attributes"
            );
        });
    }

    /// The client Guard: the client app's `Guard.py` constructs through
    /// the same entity face -- the client role registers `Entity` and the
    /// script's `BigWorld.Entity.__init__(self)` resolves.
    #[test]
    #[ignore = "runs in its own process: see SCENARIOS"]
    fn scenario_the_client_guard_entity_constructs() {
        interpreter_scenario("client", || {
            let guard_class = fixture_class("Guard");
            // SAFETY: the interpreter is up and the class stands on the
            // port's `BigWorld.Entity`.
            let guard = unsafe { construct_entity(&guard_class, 3, "Guard") }
                .expect("the engine builds the client Guard");
            stash("guard", &guard);
            assert_eq!(
                probe("__probe = repr([guard.id, guard.className, guard.isDestroyed])"),
                "[3, 'Guard', False]"
            );
        });
    }

    /// The remaining runtime faces: the create reply's `already has an
    /// associated cell entity` arm, the pending-get destroy short-circuit,
    /// the resend guard, the service refusal, and the runtime-less
    /// method refusals after a detach.
    #[test]
    #[ignore = "runs in its own process: see SCENARIOS"]
    fn scenario_the_runtime_and_destroy_faces_hold() {
        interpreter_scenario("base", || {
            let ledger = Rc::new(RefCell::new(Ledger::new(true)));
            attach_runtime(ledger.clone());
            probe(
                "import BigWorld\n\
                 class Widget(BigWorld.Base):\n\
                 \x20 pass\n\
                 class Service(BigWorld.Base):\n\
                 \x20 pass\n\
                 __probe = 'defined'",
            );
            // SAFETY: the interpreter is up.
            let widget_class = main_value("Widget");
            let widget = unsafe { construct_base(&widget_class, true, false) }
                .expect("the engine builds Widget");
            stash("widget", &widget);
            probe("widget.createInNewSpace()");
            // The create reply landed directly (the model's synchronous
            // shape): a second create now refuses on `already has`.
            // SAFETY: the instance is a live engine-built base.
            unsafe { cell_entity_registered(widget.as_ptr()) };
            assert_eq!(
                probe_error("widget.createInNewSpace()"),
                "Base.createEntityInNewSpace: Base 1 already has an \
                 associated cell entity."
            );
            // The destroy resend is swallowed, not re-sent.
            probe("widget.destroyCellEntity()");
            probe("widget.destroyCellEntity()");
            assert_eq!(
                ledger.borrow().calls,
                vec![Call::CreateInNewSpace(1, false), Call::DestroyCellEntity(1),],
                "the resend guard kept the second destroyCellEntity silent"
            );
            // A service fragment cannot register globally.
            // SAFETY: the interpreter is up.
            let service_class = main_value("Service");
            let service = unsafe { construct_base(&service_class, true, true) }
                .expect("the engine builds Service");
            stash("service", &service);
            assert_eq!(
                probe_error("service.registerGlobally('k', None)"),
                "Services cannot be registered globally"
            );
            detach_runtime();
            // Without a runtime the method faces refuse with the port's
            // internal error.
            assert_eq!(
                probe_error("widget.destroyCellEntity()"),
                "no base runtime attached"
            );
            assert_eq!(probe_error("service.destroy()"), "no base runtime attached");
        });
    }

    /// Constructing without a runtime attached refuses before any
    /// allocation -- the engine faces need the model to hand out ids.
    #[test]
    #[ignore = "runs in its own process: see SCENARIOS"]
    fn scenario_constructing_without_a_runtime_refuses() {
        interpreter_scenario("base", || {
            probe(
                "import BigWorld\n\
                 class Widget(BigWorld.Base):\n\
                 \x20 pass\n\
                 __probe = 'defined'",
            );
            // SAFETY: the interpreter is up.
            let widget_class = main_value("Widget");
            // SAFETY: the interpreter is up.
            let outcome = unsafe { construct_base(&widget_class, true, false) };
            assert!(
                matches!(outcome, Err(ConstructError::NoRuntime)),
                "no runtime, no construction"
            );
            // The refusal left nothing behind: an attach now constructs.
            let ledger = Rc::new(RefCell::new(Ledger::new(true)));
            attach_runtime(ledger.clone());
            // SAFETY: the interpreter is up.
            let widget = unsafe { construct_base(&widget_class, true, false) }
                .expect("the engine builds Widget after the attach");
            stash("widget", &widget);
            assert_eq!(probe("__probe = repr(widget.id)"), "1");
            detach_runtime();
        });
    }
}
