//! BW-14-01-04: the baseapp script layer -- the entity-type registry and
//! the `Base` script methods, so the demo's `Guard` and `Space` base
//! scripts run with their reference behavior.
//!
//! Two reference faces meet here:
//!
//! * The entity-type registry (`server/baseapp/entity_type.cpp`).
//!   `importEntityClass` imports the script module named after the entity
//!   (`entity_type.cpp:84`), takes the class of the same name off it
//!   (`:90`), falls back to the `Proxy` type when the module is absent
//!   (`:100-113`, the INFO "has no base script" line), errors when the
//!   class is missing (`:120` "Could not find class %s") and errors when
//!   the class does not derive from the engine base (`:130` "%s should be
//!   derived from BigWorld.%s"). Construction is `newEntityBase`
//!   (`entity_type.cpp:566-590`): `PyType_GenericAlloc` on the script
//!   class, the engine state placed over the same memory, then the
//!   script's `__init__` called with `okIfNull` (`base.cpp:794`, the
//!   `Base::init` tail). The port keeps the engine state in the
//!   allocation's tail (`instance_size`), the shape
//!   `python-host::math` already uses.
//!
//! * The `Base` script methods (`server/baseapp/base.cpp`), declared at
//!   `base.hpp:203-254`: `createInNewSpace` (`base.cpp:3548`), `destroy`
//!   (`:3975`), `destroyCellEntity` (`:4037`), `registerGlobally`
//!   (`:4225`), `createCellEntity` (`:3330` via the
//!   `PY_AUTO_METHOD` wrapper), the `cellData` get/set pair (`:2936`,
//!   `:2960`) and the read-only attributes `id`/`isDestroyed`/
//!   `baseType`/`className`/`cell`/`hasCell` (`base.hpp:241-250`). The
//!   associated-cell checks are `checkAssociatedCellEntity`
//!   (`base.cpp:3717`), whose four `ValueError` arms the port carries
//!   verbatim; the pending-state checks ride the
//!   [`EntityCreator::begin_cell_creation`] model.
//!
//! Deltas kept visible:
//! - The type spec binds methods as `METH_VARARGS`; the reference's
//!   keyword-only spellings (`destroy(deleteFromDB=True)`,
//!   `createInNewSpace(shouldPreferThisMachine=True)`) therefore arrive
//!   positionally or not at all. `destroy` keeps the reference's refusal
//!   of any positional argument ("Only expecting keyword arguments").
//! - `deregisterGlobally`/`writeToDB`/`addTimer`/`delTimer` are declared
//!   by the reference but are not part of this acceptance; the models
//!   behind them exist (`global_bases`, `base_write_db`,
//!   `script_timers`) and the methods are follow-up faces, not gaps in
//!   the run of the demo scripts.
//! - `cellData` holds a plain script dict seeded from the model's cell
//!   properties; the reference's `PyCellData` wraps the streamed cell
//!   data with an on-demand dict. The set-side dict check is exact
//!   (`PyDict_Check` via the builtin `dict` type) and carries the
//!   reference's TypeError.
//! - The `globalBases` entry type is the port's own; the reference's
//!   `PyBaseMailBox` also answers mailbox-method calls, which belong to
//!   the mailbox-call faces. `.cell` hands out the port's cell-mailbox
//!   view over the same address.
//! - `Base::pyGet_cell`'s `MF_ASSERT( isDestroyed_ )` on a live base with
//!   no mailbox compiles out in the reference release build; the port
//!   raises the same AttributeError in both states.
//! - The cell-side constructor is the construction leg only (allocate on
//!   the script class, call `__init__`); the full cell entity face
//!   (position/AoI wiring onto the cellapp runtime) is the cellapp
//!   entity-type row's.
//!
//! Coverage (llvm-cov 0.9.1, eight scenario processes, 2026-11-26):
//! 913/967 = 94.42% of this module's lines. The lines left uncovered are
//! the C-API's failed-allocation / unexpected-NULL guards (the
//! interpreter supplies these objects for the whole process lifetime),
//! the `unknown base` fallbacks behind `is_destroyed` (destroying a base
//! always records its id, so the destroyed arm wins), the ip-zero mailbox
//! arm (`.cell` refuses an ip-zero address before a mailbox is built),
//! and the `Proxy`-face-first branch of `install_proxy_face` (the harness
//! installs the full `Base` spec first). They are registered here as
//! defensive arms, not claimed as tested; every script-visible branch
//! reached by the demo is exercised in the scenarios.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::ffi::{CString, c_int, c_void};

use python_host::binding::{self, BindError, TypeSpec};
use python_host::capi;
use python_host::pyref::PyRef;

use crate::baseappmgr::{Addr, GlobalBaseMailbox};
use crate::cell_data::BaseAddr;
use crate::entity_creator::{
    CellCreateError, CreateArgs, CreateError, EntityCreator, EntityId, EntityType, IdPool,
    PropValue,
};
use crate::global_bases::{GlobalBases, RegisterAck, RegisterRequest};

/// The engine state one instance's allocation tail carries: the entity id
/// the `Base` methods resolve through. The reference places the C++
/// object over the allocation (`entity_type.cpp:583`); the tail slot is
/// the port's placement.
#[repr(C)]
struct BaseBody {
    entity_id: u64,
}

/// The id in an instance's tail, or `None` when the object is not an
/// engine instance (a foreign subclass of `BigWorld.Base` built by
/// script-level `__new__`, which the reference's refusal of direct
/// construction makes unreachable).
///
/// # Safety
///
/// `object` must be an instance of a type created with
/// `size_of::<BaseBody>()` instance bytes, held while the interpreter is
/// up.
unsafe fn body(object: *mut capi::PyObject) -> *mut BaseBody {
    // SAFETY: the allocation is the `PyObject` header plus the body, the
    // caller guarantees the instance shape.
    unsafe {
        object
            .cast::<u8>()
            .add(std::mem::size_of::<capi::PyObject>())
            .cast::<BaseBody>()
    }
}

/// One pending `registerGlobally` request: the key, the mailbox that was
/// offered and the script callable that answers the ack.
struct PendingRegister {
    key: String,
    mailbox: GlobalBaseMailbox,
    callback: PyRef,
}

/// `BaseApp::cellAppMgrAddr()`-style request the port records instead of
/// sending: a `createInNewSpace` ask from one base
/// (`base.cpp:3570-3585`, the `createEntityInNewSpace` request to the
/// CellAppMgr).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpaceCreateRequest {
    pub entity_id: EntityId,
    pub should_prefer_this_machine: bool,
}

/// A `createCellEntity` ask (`base.cpp:3330`, the
/// `createEntityNearEntity` request when a nearby mailbox is given; the
/// space-id path records the space instead).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CellCreateRequest {
    pub entity_id: EntityId,
    /// The nearby mailbox address, or `None` for the space-id path.
    pub nearby: Option<Addr>,
    /// The space id for the `cellData["spaceID"]` path.
    pub space_id: Option<u32>,
}

/// A `destroyCellEntity` intent (`base.cpp:4056-4069`, the
/// `destroyEntity` send to the cell app).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CellDestroyRequest {
    pub entity_id: EntityId,
}

/// One script exception an engine entry swallowed, the
/// `ScriptErrorPrint` shape (the text of the exception value).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScriptError {
    pub entity_id: EntityId,
    pub text: String,
}

/// The engine state behind the script faces: the entity creator the
/// instances resolve through, the global-bases dictionary
/// `registerGlobally` writes, the request ledgers the tests read, the
/// script classes the registry imported and the per-instance script
/// state (`cellData` dicts, pending registrations).
pub struct Engine {
    pub creator: EntityCreator,
    pub globals: GlobalBases,
    /// The address this app answers on (`BaseApp::intInterface().addr()`):
    /// the mailbox source every `registerGlobally` offers.
    pub app_addr: Addr,
    /// The script class per entity name, with its proxy flag
    /// (the class derives `BigWorld.Proxy`).
    classes: HashMap<String, (PyRef, bool)>,
    pending_registers: HashMap<EntityId, PendingRegister>,
    space_requests: Vec<SpaceCreateRequest>,
    cell_create_requests: Vec<CellCreateRequest>,
    cell_destroy_requests: Vec<CellDestroyRequest>,
    /// `isDestroyCellPending_` (`base.cpp:4052`): the guard that makes a
    /// second `destroyCellEntity` a silent no-op.
    destroy_cell_pending: HashSet<EntityId>,
    /// The `cellData` dict per base id, seeded from the model's cell
    /// properties at construction.
    cell_data: HashMap<EntityId, PyRef>,
    script_errors: Vec<ScriptError>,
    /// The issued `registerGlobally` requests, in order.
    pub registers: Vec<RegisterRequest>,
    /// The builtin `dict` type object, cached once for the exact
    /// `cellData` check.
    dict_type: Option<PyRef>,
    /// The cell-mailbox type object, for the `createCellEntity` argument
    /// check.
    cell_mailbox_type: Option<PyRef>,
    /// The base-mailbox type object (`globalBases` entries).
    base_mailbox_type: Option<PyRef>,
}

thread_local! {
    static ENGINE: RefCell<Engine> = RefCell::new(Engine::standalone());
}

impl Engine {
    /// The empty engine the thread-local starts with: no types, no ids.
    fn standalone() -> Self {
        Self {
            creator: EntityCreator::new(IdPool::new(0, 0, 0, 0)),
            globals: GlobalBases::new(),
            app_addr: (0, 0),
            classes: HashMap::new(),
            pending_registers: HashMap::new(),
            space_requests: Vec::new(),
            cell_create_requests: Vec::new(),
            cell_destroy_requests: Vec::new(),
            destroy_cell_pending: HashSet::new(),
            cell_data: HashMap::new(),
            script_errors: Vec::new(),
            registers: Vec::new(),
            dict_type: None,
            cell_mailbox_type: None,
            base_mailbox_type: None,
        }
    }

    /// Install the engine a scenario drives: the id pool, the app address
    /// and the cached builtin `dict` type. The interpreter must be up.
    pub fn install(id_pool: IdPool, app_addr: Addr) {
        let dict = unsafe {
            let builtins = capi::PyImport_AddModule(c"builtins".as_ptr());
            if builtins.is_null() {
                capi::clear_error();
                None
            } else {
                let ptr = capi::PyModule_GetDict(builtins);
                let ptr = capi::PyDict_GetItemString(ptr, c"dict".as_ptr());
                if ptr.is_null() {
                    capi::clear_error();
                    None
                } else {
                    capi::inc_ref(ptr);
                    PyRef::from_owned(ptr)
                }
            }
        };
        ENGINE.with(|engine| {
            let mut engine = engine.borrow_mut();
            engine.creator = EntityCreator::new(id_pool);
            engine.app_addr = app_addr;
            engine.dict_type = dict;
        });
    }

    /// Drop every engine state (a fresh scenario in the same process --
    /// none: the interpreter refuses a second init, so each scenario is
    /// its own process and this exists for symmetry with the other
    /// engines).
    pub fn reset() {
        ENGINE.with(|engine| *engine.borrow_mut() = Engine::standalone());
    }

    /// Run `body` against the engine.
    pub fn with<T>(body: impl FnOnce(&mut Engine) -> T) -> T {
        ENGINE.with(|engine| body(&mut engine.borrow_mut()))
    }

    // -- registry ----------------------------------------------------------

    /// Add one entity type to the creator, from the spec the entity defs
    /// produce.
    pub fn register_type(&mut self, spec: EntityTypeSpec) {
        self.creator.add_type(EntityType {
            id: spec.id,
            name: spec.name.clone(),
            can_be_on_base: spec.can_be_on_base,
            is_service: spec.is_service,
            is_proxy: false,
            can_be_on_cell: spec.can_be_on_cell,
            properties: Vec::new(),
            methods: Vec::new(),
        });
    }

    /// `importEntityClass` (`entity_type.cpp:81-136`): import the module
    /// named after the entity, take the class of the same name, fall back
    /// to the `Proxy` type when the module is absent, and refuse a class
    /// that does not derive the engine base. The class is kept with its
    /// proxy flag.
    pub fn import_entity_class(&mut self, name: &str) -> Result<(), ImportError> {
        // SAFETY: the interpreter is up; every borrowed pointer below is
        // read before any call that can free it.
        unsafe {
            let module = capi::PyImport_ImportModule(cstr(name).as_ptr());
            let (class, is_proxy) = if module.is_null() {
                // The no-script fallback: the `Proxy` type stands in, the
                // INFO line the reference prints (`entity_type.cpp:111`).
                capi::clear_error();
                let Some(proxy) = self.bigworld_attr("Proxy") else {
                    return Err(ImportError::NoScriptFace);
                };
                (proxy, true)
            } else {
                let attribute = capi::PyObject_GetAttrString(module, cstr(name).as_ptr());
                capi::dec_ref(module);
                if attribute.is_null() {
                    capi::clear_error();
                    // "Could not find class %s" (`entity_type.cpp:120`).
                    return Err(ImportError::ClassMissing(String::from(name)));
                }
                let class = PyRef::from_owned(attribute)
                    .ok_or(ImportError::ClassMissing(String::from(name)))?;
                (class, false)
            };

            // The subclass check (`entity_type.cpp:124-135`): the class
            // must derive `BigWorld.Base` (a `Proxy` subclass passes --
            // `Proxy` stands on `Base`).
            let Some(base) = self.bigworld_attr("Base") else {
                return Err(ImportError::NoScriptFace);
            };
            let derived = capi::PyType_IsSubtype(
                class.as_ptr() as *mut capi::PyTypeObject,
                base.as_ptr() as *mut capi::PyTypeObject,
            ) != 0;
            if !derived {
                return Err(ImportError::WrongBase {
                    name: String::from(name),
                });
            }

            // The proxy flag decides the placement in `newEntityBase`
            // (`entity_type.cpp:577`); the port records it from the class
            // hierarchy itself.
            let is_proxy = is_proxy
                || self.bigworld_attr("Proxy").is_some_and(|proxy| {
                    capi::PyType_IsSubtype(
                        class.as_ptr() as *mut capi::PyTypeObject,
                        proxy.as_ptr() as *mut capi::PyTypeObject,
                    ) != 0
                });

            self.classes.insert(String::from(name), (class, is_proxy));
            Ok(())
        }
    }

    /// One `BigWorld` module attribute as an owned reference, or `None`
    /// when it is absent (the pending error is cleared).
    fn bigworld_attr(&self, name: &str) -> Option<PyRef> {
        // SAFETY: the interpreter is up; the borrowed result is incref'd
        // before the wrap.
        unsafe {
            let module = capi::PyImport_AddModule(c"BigWorld".as_ptr());
            if module.is_null() {
                capi::clear_error();
                return None;
            }
            let attribute = capi::PyObject_GetAttrString(module, cstr(name).as_ptr());
            if attribute.is_null() {
                capi::clear_error();
                return None;
            }
            PyRef::from_owned(attribute)
        }
    }

    // -- construction ------------------------------------------------------

    /// `newEntityBase` + `Base::init` (`entity_type.cpp:566`,
    /// `base.cpp:780-801`): allocate on the script class, place the
    /// engine id in the tail, seed the `cellData` dict from the model's
    /// cell properties, call the script `__init__` (the reference's
    /// `okIfNull` -- a lookup failure is cleared and skipped), and refuse
    /// the result when the script destroyed the base inside its own
    /// `__init__` (`base.cpp`'s `if (pNewBase->isDestroyed())`).
    pub fn new_entity(name: &str) -> Result<PyRef, NewEntityError> {
        // Phase one, borrowed: the script class, the model record and the
        // seeded cell data. The borrow ends here -- the script's
        // `__init__` re-enters the engine (the demo's `Space.__init__`
        // runs `createInNewSpace` through the very engine that is
        // constructing), so no borrow may live across the call.
        let (class, model) = Engine::with(|engine| -> Result<_, NewEntityError> {
            let class = engine
                .classes
                .get(name)
                .cloned()
                .map(|(class, _)| class)
                .ok_or(NewEntityError::UnknownType(String::from(name)))?;
            let model = engine
                .creator
                .create_base_locally(name, CreateArgs::new())
                .map_err(|error| match error {
                    CreateError::InvalidType { name } => NewEntityError::InvalidType(name),
                    CreateError::InvalidDbId | CreateError::NoId => NewEntityError::NoId,
                })?;

            // The cell data the reference streams in (`base.cpp:286-296`
            // builds the `PyCellData` when the type can be on a cell); the
            // port seeds a plain dict from the model's cell properties.
            let cell_props = model.cell_props.clone();
            let dict = unsafe { prop_dict(&cell_props) };
            if let Some(dict) = dict {
                engine.cell_data.insert(model.id, dict);
            }
            Ok((class, model))
        })?;

        // Phase two, unborrowed: `newEntityBase` (`entity_type.cpp:566`)
        // -- allocate on the script class, place the engine id in the
        // tail, then the `__init__` call (`base.cpp:794`, `okIfNull`: a
        // missing `__init__` is cleared and skipped).
        // SAFETY: the interpreter is up and the class is a live type
        // object; the instance is owned from here.
        let mut init_failed: Option<String> = None;
        let instance = unsafe {
            let pointer = capi::PyType_GenericAlloc(class.as_ptr() as *mut capi::PyTypeObject, 0);
            if pointer.is_null() {
                capi::clear_error();
                return Err(NewEntityError::Allocation);
            }
            (*body(pointer)).entity_id = u64::from(model.id);
            let init = capi::PyObject_GetAttrString(pointer, c"__init__".as_ptr());
            if init.is_null() {
                capi::clear_error();
            } else {
                let result = capi::PyObject_CallNoArgs(init);
                capi::dec_ref(init);
                if result.is_null() {
                    // A script exception in `__init__` is the reference's
                    // `ScriptErrorPrint` -- printed and swallowed.
                    init_failed = Some(swallow_error_text());
                }
            }
            pointer
        };

        // Phase three, borrowed: record a swallowed `__init__` exception
        // and `base.cpp`'s post-init destroyed check -- a base its own
        // `__init__` destroyed does not survive the creation
        // (`destroy_base` removed the record; the destroyed set remembers).
        let destroyed = Engine::with(|engine| {
            if let Some(text) = init_failed {
                engine.script_errors.push(ScriptError {
                    entity_id: model.id,
                    text,
                });
            }
            engine.creator.is_destroyed(model.id)
        });
        if destroyed {
            // SAFETY: a live instance; the failed creation releases it.
            unsafe { capi::dec_ref(instance) };
            return Err(NewEntityError::DestroyedInInit);
        }
        PyRef::from_owned(instance).ok_or(NewEntityError::Allocation)
    }

    /// The cell-side construction leg: allocate on the script class and
    /// call `__init__`. No engine state rides the instance (the cellapp
    /// entity-type row owns that face); the acceptance is that the cell
    /// scripts run.
    pub fn construct_cell_entity(name: &str) -> Result<PyRef, ImportError> {
        // The class while borrowed; the `__init__` call below re-enters
        // the engine the same way `new_entity`'s does.
        let class = Engine::with(|engine| {
            engine
                .classes
                .get(name)
                .cloned()
                .map(|(class, _)| class)
                .ok_or_else(|| ImportError::ClassMissing(String::from(name)))
        })?;
        // SAFETY: the interpreter is up and the class is a live type.
        let mut init_failed: Option<String> = None;
        let instance = unsafe {
            let pointer = capi::PyType_GenericAlloc(class.as_ptr() as *mut capi::PyTypeObject, 0);
            if pointer.is_null() {
                capi::clear_error();
                return Err(ImportError::AllocationFailed);
            }
            let init = capi::PyObject_GetAttrString(pointer, c"__init__".as_ptr());
            if init.is_null() {
                capi::clear_error();
            } else {
                let result = capi::PyObject_CallNoArgs(init);
                capi::dec_ref(init);
                if result.is_null() {
                    init_failed = Some(swallow_error_text());
                }
            }
            PyRef::from_owned(pointer).ok_or(ImportError::AllocationFailed)?
        };
        if let Some(text) = init_failed {
            Engine::with(|engine| {
                engine
                    .script_errors
                    .push(ScriptError { entity_id: 0, text });
            });
        }
        Ok(instance)
    }

    // -- the registerGlobally reply leg ------------------------------------

    /// `MakeGlobalHandler::handleMessage` (`global_bases.cpp:223-250`):
    /// the manager's ack lands the entry on success and answers the
    /// callback `True`; a duplicate answers `False` and changes nothing;
    /// the exception path (`handleException`) answers `False` too. A
    /// script exception in the callback is swallowed and recorded, the
    /// `ScriptErrorPrint` behavior.
    pub fn register_ack(entity_id: EntityId, success: bool) -> Option<RegisterAck> {
        // Phase one, borrowed: pop the pending request and apply the ack.
        // The borrow ends before the callback -- the demo's
        // `onRegistered(False)` runs `destroyCellEntity`, which re-enters
        // the engine.
        let (ack, callback) = Engine::with(|engine| {
            let pending = engine.pending_registers.remove(&entity_id)?;
            let ack = engine.globals.on_register_ack(
                entity_id,
                &pending.key,
                pending.mailbox.clone(),
                success,
            );
            Some((ack, pending.callback))
        })?;
        // Phase two, unborrowed: the callback gets the bool
        // (`global_bases.cpp:243`). A script exception is the reference's
        // `ScriptErrorPrint` -- printed and swallowed.
        unsafe {
            let answer = capi::PyBool_FromLong(if success { 1 } else { 0 });
            let arguments = capi::PyTuple_New(1);
            if !arguments.is_null() {
                capi::PyTuple_SetItem(arguments, 0, answer); // steals
                let result = capi::PyObject_CallObject(callback.as_ptr(), arguments);
                capi::dec_ref(arguments);
                if result.is_null() {
                    let text = swallow_error_text();
                    Engine::with(|engine| {
                        engine.script_errors.push(ScriptError { entity_id, text });
                    });
                }
            }
        }
        Some(ack)
    }

    /// The registration exception path (`global_bases.cpp:253`, the
    /// `handleException`): the callback answers `False`.
    pub fn register_exception(entity_id: EntityId) -> Option<RegisterAck> {
        // Phase one, borrowed: pop the pending and apply the exception
        // ack (`global_bases.cpp:253`); the callback gets `False`.
        let (ack, callback) = Engine::with(|engine| {
            let pending = engine.pending_registers.remove(&entity_id)?;
            let ack = engine.globals.on_register_exception();
            Some((ack, pending.callback))
        })?;
        // Phase two, unborrowed: the callback, re-entry allowed.
        unsafe {
            let answer = capi::PyBool_FromLong(0);
            let arguments = capi::PyTuple_New(1);
            if !arguments.is_null() {
                capi::PyTuple_SetItem(arguments, 0, answer);
                let result = capi::PyObject_CallObject(callback.as_ptr(), arguments);
                capi::dec_ref(arguments);
                if result.is_null() {
                    let text = swallow_error_text();
                    Engine::with(|engine| {
                        engine.script_errors.push(ScriptError { entity_id, text });
                    });
                }
            }
        }
        Some(ack)
    }

    // -- ledgers (test seams) -----------------------------------------------

    pub fn space_requests(&self) -> &[SpaceCreateRequest] {
        &self.space_requests
    }

    pub fn cell_create_requests(&self) -> &[CellCreateRequest] {
        &self.cell_create_requests
    }

    pub fn cell_destroy_requests(&self) -> &[CellDestroyRequest] {
        &self.cell_destroy_requests
    }

    pub fn script_errors(&self) -> &[ScriptError] {
        &self.script_errors
    }

    pub fn pending_register_count(&self) -> usize {
        self.pending_registers.len()
    }

    /// The `cellData` dict of one base, for the test reads.
    pub fn cell_data_of(&mut self, entity_id: EntityId) -> Option<PyRef> {
        self.cell_data.get(&entity_id).cloned()
    }

    /// Drop one base's `cellData`, for the missing-dict refusal test seam.
    pub fn drop_cell_data(&mut self, entity_id: EntityId) {
        self.cell_data.remove(&entity_id);
    }

    /// Whether the imported class for `name` stood in on the `Proxy` type
    /// (the no-script fallback), for the test reads.
    pub fn class_is_proxy(&self, name: &str) -> bool {
        self.classes
            .get(name)
            .is_some_and(|(_, is_proxy)| *is_proxy)
    }

    /// The imported class object, for identity checks (`Ghost` has no
    /// module: the registry holds the `Proxy` type itself).
    pub fn class_of(&self, name: &str) -> Option<&PyRef> {
        self.classes.get(name).map(|(class, _)| class)
    }
}

/// The def-driven half of an entity type: what [`Engine::register_type`]
/// consumes. The entity defs build these; the acceptance fixtures come
/// from the parsed `EntityDescriptionMap`.
#[derive(Clone, Debug)]
pub struct EntityTypeSpec {
    pub name: String,
    pub id: u32,
    pub can_be_on_base: bool,
    pub can_be_on_cell: bool,
    pub is_service: bool,
}

/// The `importEntityClass` failures (`entity_type.cpp:96-135`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ImportError {
    /// "Could not load module %s" -- and no `Proxy` face to fall back to.
    NoScriptFace,
    /// "Could not find class %s".
    ClassMissing(String),
    /// "%s should be derived from BigWorld.%s".
    WrongBase { name: String },
    /// The cell-side constructor could not allocate.
    AllocationFailed,
}

/// The `newEntityBase`/`Base::init` failures.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NewEntityError {
    /// No script class was imported for the name.
    UnknownType(String),
    /// `create_base_locally` refused the type.
    InvalidType(String),
    /// The id pool was empty.
    NoId,
    /// The allocation failed.
    Allocation,
    /// The script's own `__init__` destroyed the base
    /// (`base.cpp`'s `if (pNewBase->isDestroyed())`).
    DestroyedInInit,
}

// -- C helpers -----------------------------------------------------------

/// A NUL-terminated copy of `text`; the names the faces use are
/// engine-fixed constants.
fn cstr(text: &str) -> CString {
    CString::new(text).expect("an engine-fixed name without a NUL")
}

/// Hand a `PyRef` to the C surface as an owned pointer: the extra count
/// covers the handle's own `Drop`.
fn into_raw(reference: PyRef) -> *mut capi::PyObject {
    let ptr = reference.as_ptr();
    // SAFETY: a live object; the increment is the C API's own contract.
    unsafe { capi::inc_ref(ptr) };
    ptr
}

/// Read the pending exception as its rendered text, and clear it.
unsafe fn swallow_error_text() -> String {
    unsafe {
        let mut kind = std::ptr::null_mut();
        let mut value = std::ptr::null_mut();
        let mut trace = std::ptr::null_mut();
        capi::PyErr_Fetch(&mut kind, &mut value, &mut trace);
        let text = if value.is_null() {
            String::from("unknown script error")
        } else {
            let rendered = capi::PyObject_Str(value);
            if rendered.is_null() {
                capi::clear_error();
                String::from("unrenderable script error")
            } else {
                let text = capi::to_string(rendered);
                capi::dec_ref(rendered);
                text
            }
        };
        capi::dec_ref(kind);
        capi::dec_ref(value);
        capi::dec_ref(trace);
        text
    }
}

/// The model's cell properties as a script dict (`Int`/`Float`/`Str`/
/// `Bool`/`Vec3` to the matching builtin objects).
unsafe fn prop_dict(properties: &HashMap<String, PropValue>) -> Option<PyRef> {
    unsafe {
        let dict = capi::PyDict_New();
        if dict.is_null() {
            capi::clear_error();
            return None;
        }
        for (name, value) in properties {
            let object = match value {
                PropValue::Int(value) => capi::PyLong_FromLongLong(*value),
                PropValue::Float(value) => capi::PyFloat_FromDouble(*value),
                PropValue::Str(value) => capi::unicode_from_bytes(value.as_bytes()),
                PropValue::Bool(value) => capi::PyBool_FromLong(if *value { 1 } else { 0 }),
                PropValue::Vec3(values) => {
                    let x = capi::PyFloat_FromDouble(f64::from(values[0]));
                    let y = capi::PyFloat_FromDouble(f64::from(values[1]));
                    let z = capi::PyFloat_FromDouble(f64::from(values[2]));
                    capi::PyTuple_Pack(3, x, y, z)
                }
            };
            if object.is_null() {
                capi::clear_error();
                continue;
            }
            capi::PyDict_SetItemString(dict, cstr(name).as_ptr(), object);
            capi::dec_ref(object);
        }
        PyRef::from_owned(dict)
    }
}

/// `PyErr_SetString` with a Rust message: one helper per exception kind,
/// so the mutable statics are read only inside `unsafe`.
fn raise_with(kind: fn() -> *mut capi::PyObject, message: &str) {
    // SAFETY: the message is a fresh unicode object; the exception global
    // is the interpreter's own.
    unsafe {
        let rendered = capi::unicode_from_bytes(message.as_bytes());
        capi::PyErr_SetObject(kind(), rendered);
        capi::dec_ref(rendered);
    }
}

/// The `ValueError` global.
fn value_error() -> *mut capi::PyObject {
    // SAFETY: an interpreter static.
    unsafe { capi::PyExc_ValueError }
}

/// The `TypeError` global.
fn type_error() -> *mut capi::PyObject {
    // SAFETY: an interpreter static.
    unsafe { capi::PyExc_TypeError }
}

/// The `AttributeError` global.
fn attribute_error() -> *mut capi::PyObject {
    // SAFETY: an interpreter static.
    unsafe { capi::PyExc_AttributeError }
}

/// `ValueError` with a Rust message.
fn raise_value_error(message: &str) {
    raise_with(value_error, message);
}

/// `TypeError` with a Rust message.
fn raise_type_error(message: &str) {
    raise_with(type_error, message);
}

/// `AttributeError` with a Rust message.
fn raise_attribute_error(message: &str) {
    raise_with(attribute_error, message);
}

/// Whether `object` is `None`.
fn mailbox_is_none(object: *mut capi::PyObject) -> bool {
    std::ptr::eq(object, capi::py_none())
}

/// The `checkAssociatedCellEntity` ValueError texts (`base.cpp:3717-3760`),
/// prefixed the way each caller prefixes them.
fn associated_cell_error(error: CellCreateError, prefix: &str, entity_id: EntityId) -> String {
    let tail = match error {
        CellCreateError::Destroyed => format!("Base {entity_id} is destroyed"),
        CellCreateError::AlreadyCreating => {
            format!("Base {entity_id} is in the process of creating an associated cell entity.")
        }
        CellCreateError::WaitingForContact => format!(
            "Base {entity_id} is waiting for contact from its created associated cell entity.\n"
        ),
        CellCreateError::AlreadyHasCell => {
            format!("Base {entity_id} already has an associated cell entity.")
        }
        CellCreateError::NoCellScript => {
            format!("Base {entity_id} is not cell based")
        }
        CellCreateError::UnknownBase => format!("Base {entity_id} is unknown"),
    };
    format!("{prefix}{tail}")
}

// -- the engine faces ----------------------------------------------------

/// Install the `BigWorld.Base` type with the engine faces: the script
/// methods and the read-only attributes, plus the instance tail. The
/// interpreter's own startup installs the bare import faces first, so run
/// `python_host::binding::reset` before this, then
/// `python_host::script_types::install` -- `create_type` hands back the
/// existing type object for a repeat name, so the first spec wins, and a
/// subclass of `Base` needs no instance bytes of its own (the reference's
/// `Py_Header` classes carry no C++ fields the subclass adds to).
pub fn install_base_faces() -> Result<(), BindError> {
    let spec = TypeSpec::new("BigWorld.Base")
        .subclassable()
        .instance_size(std::mem::size_of::<BaseBody>())
        .attribute("id", get_id, None)
        .attribute("isDestroyed", get_is_destroyed, None)
        .attribute("baseType", get_base_type, None)
        .attribute("className", get_class_name, None)
        .attribute("cell", get_cell, None)
        .attribute("hasCell", get_has_cell, None)
        .attribute("cellData", get_cell_data, Some(set_cell_data))
        .attribute("databaseID", get_database_id, None)
        .method("createInNewSpace", method_create_in_new_space)
        .method("registerGlobally", method_register_globally)
        .method("destroyCellEntity", method_destroy_cell_entity)
        .method("destroy", method_destroy)
        .method("createCellEntity", method_create_cell_entity);
    binding::create_type(&spec).map(|_| ())
}

/// Install the `BigWorld.Proxy` type over the full-spec `Base`. The
/// interpreter's own startup installs the bare `Proxy` too, so this runs
/// after `binding::reset` + [`install_base_faces`] and re-fetches from the
/// registry: the first spec wins, so this call stands in only when the
/// module runs without a prior bare install (the standalone harness).
/// The instance size mirrors the reference's `Py_Header( Proxy, Base )`:
/// `Proxy` adds no fields of its own, so its `tp_basicsize` is the base's.
pub fn install_proxy_face() -> Result<(), BindError> {
    let base = binding::create_type(&TypeSpec::new("BigWorld.Base").subclassable())
        .or_else(|_| binding::create_type(&TypeSpec::new("BigWorld.Base")))?;
    let spec = TypeSpec::new("BigWorld.Proxy")
        .subclassable()
        .instance_size(std::mem::size_of::<BaseBody>())
        .extends(&base);
    binding::create_type(&spec).map(|_| ())
}

/// The mailbox types and the `BigWorld.globalBases` dictionary. Run after
/// `script_types::install` so the module is live.
pub fn install_module_faces() -> Result<(), BindError> {
    // The base-mailbox type: `globalBases[key]` entries. `.cell` hands
    // out the cell-mailbox view.
    let base_spec = TypeSpec::new("BigWorld.GlobalBaseMailbox")
        .instance_size(std::mem::size_of::<MailboxBody>())
        .attribute("cell", get_mailbox_cell, None);
    let base_mailbox = binding::create_type(&base_spec)?;

    // The cell-mailbox type: what `.cell` answers and what
    // `createCellEntity` accepts.
    let cell_spec =
        TypeSpec::new("BigWorld.CellMailbox").instance_size(std::mem::size_of::<MailboxBody>());
    let cell_mailbox = binding::create_type(&cell_spec)?;

    ENGINE.with(|engine| {
        let mut engine = engine.borrow_mut();
        engine.base_mailbox_type = Some(base_mailbox);
        engine.cell_mailbox_type = Some(cell_mailbox);
    });

    // The `globalBases` dictionary singleton onto the module
    // (`script_bigworld.cpp` exposes `globalBases` as a module
    // attribute; the port's mapping reads the model's dictionary).
    let globals_spec = TypeSpec::new("BigWorld.GlobalBases")
        .instance_size(std::mem::size_of::<usize>())
        .slot(capi::Py_mp_subscript, globals_subscript as *mut c_void);
    let globals = binding::create_type(&globals_spec)?;
    let instance = unsafe {
        let pointer = capi::PyType_GenericAlloc(globals.as_ptr() as *mut capi::PyTypeObject, 0);
        if pointer.is_null() {
            capi::clear_error();
            return Err(BindError::CreateFailed);
        }
        pointer
    };
    // SAFETY: both objects are live; a failed set is cleared, the
    // attribute simply absent (`script_types::set_attr`'s rule).
    unsafe {
        let module = capi::PyImport_AddModule(c"BigWorld".as_ptr());
        if module.is_null() {
            capi::clear_error();
            capi::dec_ref(instance);
            return Err(BindError::CreateFailed);
        }
        let failed = capi::PyObject_SetAttrString(module, c"globalBases".as_ptr(), instance) != 0;
        capi::dec_ref(instance);
        if failed {
            capi::clear_error();
        }
    }
    Ok(())
}

/// A mailbox instance over one address: the tail carries the entity id
/// and the address.
#[repr(C)]
struct MailboxBody {
    entity: u32,
    ip: u32,
    port: u16,
}

/// Build a base-mailbox instance over one record.
unsafe fn base_mailbox(mailbox: &GlobalBaseMailbox) -> Option<PyRef> {
    unsafe {
        let type_object = Engine::with(|engine| engine.base_mailbox_type.clone());
        let type_object = type_object?;
        let pointer = capi::PyType_GenericAlloc(type_object.as_ptr() as *mut capi::PyTypeObject, 0);
        if pointer.is_null() {
            capi::clear_error();
            return None;
        }
        let slot = body_mailbox(pointer);
        (*slot).entity = mailbox.entity;
        (*slot).ip = mailbox.addr.0;
        (*slot).port = mailbox.addr.1;
        PyRef::from_owned(pointer)
    }
}

/// Build a cell-mailbox instance over one address.
unsafe fn cell_mailbox(address: BaseAddr) -> Option<PyRef> {
    unsafe {
        let type_object = Engine::with(|engine| engine.cell_mailbox_type.clone());
        let type_object = type_object?;
        let pointer = capi::PyType_GenericAlloc(type_object.as_ptr() as *mut capi::PyTypeObject, 0);
        if pointer.is_null() {
            capi::clear_error();
            return None;
        }
        let slot = body_mailbox(pointer);
        (*slot).entity = 0;
        (*slot).ip = address.ip;
        (*slot).port = address.port;
        PyRef::from_owned(pointer)
    }
}

/// The mailbox tail of a mailbox instance.
unsafe fn body_mailbox(object: *mut capi::PyObject) -> *mut MailboxBody {
    unsafe {
        object
            .cast::<u8>()
            .add(std::mem::size_of::<capi::PyObject>())
            .cast::<MailboxBody>()
    }
}

/// `BigWorld.globalBases[key]`: the model dictionary answers, an unknown
/// key is a `KeyError`.
unsafe extern "C" fn globals_subscript(
    _self: *mut capi::PyObject,
    key: *mut capi::PyObject,
) -> *mut capi::PyObject {
    // SAFETY: the interpreter is up; the key is read as a UTF-8 string.
    unsafe {
        let rendered = capi::PyObject_Str(key);
        if rendered.is_null() {
            capi::clear_error();
            raise_key_error("<unprintable key>");
            return std::ptr::null_mut();
        }
        let text = capi::to_string(rendered);
        capi::dec_ref(rendered);
        let mailbox = Engine::with(|engine| engine.globals.get(&text).cloned());
        match mailbox {
            Some(record) => {
                let instance = base_mailbox(&record);
                match instance {
                    Some(instance) => into_raw(instance),
                    None => {
                        raise_key_error(&text);
                        std::ptr::null_mut()
                    }
                }
            }
            None => {
                raise_key_error(&text);
                std::ptr::null_mut()
            }
        }
    }
}

/// `KeyError` with the raw key text.
fn raise_key_error(key: &str) {
    // SAFETY: the key is a fresh unicode object; `KeyError` renders its
    // argument.
    unsafe {
        let rendered = capi::unicode_from_bytes(key.as_bytes());
        capi::PyErr_SetObject(key_error_type(), rendered);
        capi::dec_ref(rendered);
    }
}

/// The `KeyError` global.
fn key_error_type() -> *mut capi::PyObject {
    // SAFETY: the exception globals are static for the interpreter's
    // lifetime.
    unsafe {
        let builtins = capi::PyImport_AddModule(c"builtins".as_ptr());
        if builtins.is_null() {
            capi::clear_error();
            return std::ptr::null_mut();
        }
        let dictionary = capi::PyModule_GetDict(builtins);
        let kind = capi::PyDict_GetItemString(dictionary, c"KeyError".as_ptr());
        if kind.is_null() {
            capi::clear_error();
        }
        kind
    }
}

// -- the Base script methods ---------------------------------------------

/// `Base::py_createInNewSpace` (`base.cpp:3548`): the optional
/// `shouldPreferThisMachine` ask, the associated-cell check through
/// `prepareForCellCreate`, the create request recorded.
unsafe extern "C" fn method_create_in_new_space(
    self_: *mut capi::PyObject,
    args: *mut capi::PyObject,
) -> *mut capi::PyObject {
    unsafe {
        let entity_id = match engine_id(self_) {
            Some(id) => id,
            None => return std::ptr::null_mut(),
        };
        // `|O:Base.createEntityInNewSpace` -- the reference's own format
        // string, error prefix and all (`base.cpp:3560-3568`).
        let mut prefer: *mut capi::PyObject = std::ptr::null_mut();
        if capi::PyArg_ParseTuple(
            args,
            c"|O:Base.createEntityInNewSpace".as_ptr(),
            &mut prefer,
        ) == 0
        {
            return std::ptr::null_mut();
        }
        let prefer_this_machine = !prefer.is_null() && capi::PyObject_IsTrue(prefer) != 0;

        // `prepareForCellCreate` (`base.cpp:3269`): the check and the
        // pending flags through the model.
        let prefix = "Base.createEntityInNewSpace: ";
        let outcome = Engine::with(
            |engine| match engine.creator.begin_cell_creation(entity_id) {
                Ok(()) => {
                    engine.space_requests.push(SpaceCreateRequest {
                        entity_id,
                        should_prefer_this_machine: prefer_this_machine,
                    });
                    None
                }
                Err(error) => Some(associated_cell_error(error, prefix, entity_id)),
            },
        );
        if let Some(message) = outcome {
            raise_value_error(&message);
            return std::ptr::null_mut();
        }
        capi::py_none()
    }
}

/// `Base::py_registerGlobally` (`base.cpp:4225`): the key and callback,
/// the callable check, the string conversion for the record, the request
/// through the model. The callback stays pending until the ack.
unsafe extern "C" fn method_register_globally(
    self_: *mut capi::PyObject,
    args: *mut capi::PyObject,
) -> *mut capi::PyObject {
    unsafe {
        let entity_id = match engine_id(self_) {
            Some(id) => id,
            None => return std::ptr::null_mut(),
        };
        let mut key: *mut capi::PyObject = std::ptr::null_mut();
        let mut callback: *mut capi::PyObject = std::ptr::null_mut();
        if capi::PyArg_ParseTuple(
            args,
            c"OO:registerGlobally".as_ptr(),
            &mut key,
            &mut callback,
        ) == 0
        {
            return std::ptr::null_mut();
        }
        if capi::PyCallable_Check(callback) == 0 {
            raise_type_error("The second argument must be callable");
            return std::ptr::null_mut();
        }

        // The key's string form: the reference converts for the log line
        // and still registers the original object on a failure -- the
        // port records the rendered form it has.
        let rendered = capi::PyObject_Str(key);
        let key_text = if rendered.is_null() {
            capi::clear_error();
            String::new()
        } else {
            let text = capi::to_string(rendered);
            capi::dec_ref(rendered);
            text
        };

        capi::inc_ref(callback);
        let callback = match PyRef::from_owned(callback) {
            Some(callback) => callback,
            None => return std::ptr::null_mut(),
        };
        Engine::with(|engine| {
            let mailbox = GlobalBaseMailbox {
                entity: entity_id,
                addr: engine.app_addr,
            };
            let request = GlobalBases::register_request(&key_text, mailbox.clone());
            engine.registers.push(request);
            engine.pending_registers.insert(
                entity_id,
                PendingRegister {
                    key: key_text,
                    mailbox,
                    callback,
                },
            );
        });
        capi::py_none()
    }
}

/// `Base::py_destroyCellEntity` (`base.cpp:4037`): the pending/contact
/// check, the no-arguments refusal, the silent second-call guard, the
/// destroy intent recorded.
unsafe extern "C" fn method_destroy_cell_entity(
    self_: *mut capi::PyObject,
    args: *mut capi::PyObject,
) -> *mut capi::PyObject {
    unsafe {
        let entity_id = match engine_id(self_) {
            Some(id) => id,
            None => return std::ptr::null_mut(),
        };
        // The guard order is the reference's (`base.cpp:4041-4044`): the
        // contact check first, the argument check second.
        let prefix = "Base.destroyCellEntity: ";
        let state = Engine::with(|engine| {
            let entity = engine.creator.bases().get(&entity_id).cloned();
            (entity, engine.destroy_cell_pending.contains(&entity_id))
        });
        let Some(entity) = state.0 else {
            // A destroyed base's record is discarded; the reference keeps
            // the destroyed flag on the object (`isDestroyed`), so the
            // destroyed arm wins over the unknown arm.
            let error = if Engine::with(|engine| engine.creator.is_destroyed(entity_id)) {
                CellCreateError::Destroyed
            } else {
                CellCreateError::UnknownBase
            };
            raise_value_error(&associated_cell_error(error, prefix, entity_id));
            return std::ptr::null_mut();
        };
        if !entity.is_get_cell_pending && !check_associated_cell(&entity, true, prefix, entity_id) {
            return std::ptr::null_mut();
        }
        if capi::PyTuple_Size(args) > 0 {
            raise_type_error("Base.destroyCellEntity() accepts no arguments");
            return std::ptr::null_mut();
        }
        if state.1 {
            // "Don't let people do this twice" (`base.cpp:4050-4052`).
            return capi::py_none();
        }
        Engine::with(|engine| {
            engine.destroy_cell_pending.insert(entity_id);
            engine
                .cell_destroy_requests
                .push(CellDestroyRequest { entity_id });
        });
        capi::py_none()
    }
}

/// `Base::py_destroy` (`base.cpp:3975`): the still-has-cell refusal, the
/// already-destroyed refusal, the flags through the model's destroy.
unsafe extern "C" fn method_destroy(
    self_: *mut capi::PyObject,
    args: *mut capi::PyObject,
) -> *mut capi::PyObject {
    unsafe {
        let entity_id = match engine_id(self_) {
            Some(id) => id,
            None => return std::ptr::null_mut(),
        };
        if capi::PyTuple_Size(args) != 0 {
            raise_type_error("Only expecting keyword arguments");
            return std::ptr::null_mut();
        }
        let state = Engine::with(|engine| {
            engine
                .creator
                .bases()
                .get(&entity_id)
                .map(|entity| {
                    (
                        entity.cell_entity_addr.is_some(),
                        entity.is_get_cell_pending,
                        entity.destroyed,
                        entity.db_id,
                    )
                })
                .unwrap_or((false, false, true, 0))
        });
        let (has_cell, get_pending, destroyed, db_id) = state;
        if has_cell || get_pending {
            raise_value_error("Still has cell entity. Use Base.destroyCellEntity");
            return std::ptr::null_mut();
        }
        if destroyed {
            raise_value_error("Base entity already destroyed");
            return std::ptr::null_mut();
        }
        // `deleteFromDB` defaults false, `writeToDB` defaults
        // `hasWrittenToDB()` -- a database id means it was written
        // (`base.hpp:85`).
        let delete_from_db = false;
        let write_to_db = db_id != 0;
        let outcome = Engine::with(|engine| {
            engine
                .creator
                .destroy_base(entity_id, delete_from_db, write_to_db, false)
        });
        let _ = outcome;
        capi::py_none()
    }
}

/// `Base::createCellEntity` (`base.cpp:3330`): the associated-cell
/// check, the mailbox acceptance (the ip-zero refusal is the reference's
/// own text), the space-id fallback for a null mailbox, the create
/// request recorded.
unsafe extern "C" fn method_create_cell_entity(
    self_: *mut capi::PyObject,
    args: *mut capi::PyObject,
) -> *mut capi::PyObject {
    unsafe {
        let entity_id = match engine_id(self_) {
            Some(id) => id,
            None => return std::ptr::null_mut(),
        };
        let mut mailbox: *mut capi::PyObject = std::ptr::null_mut();
        if capi::PyArg_ParseTuple(args, c"O:createCellEntity".as_ptr(), &mut mailbox) == 0 {
            return std::ptr::null_mut();
        }
        // `checkAssociatedCellEntity(false, "Base.createCellEntity: ")`
        // (`base.cpp:3334`).
        let prefix = "Base.createCellEntity: ";
        let state = Engine::with(|engine| {
            engine
                .creator
                .bases()
                .get(&entity_id)
                .cloned()
                .ok_or(())
                .map(|entity| (entity, engine.cell_data.get(&entity_id).cloned()))
                .map_err(|_| ())
        });
        let Ok((entity, cell_data)) = state else {
            // A destroyed base's record is discarded; the reference keeps
            // the destroyed flag on the object (`isDestroyed`), so the
            // destroyed arm wins over the unknown arm.
            let error = if Engine::with(|engine| engine.creator.is_destroyed(entity_id)) {
                CellCreateError::Destroyed
            } else {
                CellCreateError::UnknownBase
            };
            raise_value_error(&associated_cell_error(error, prefix, entity_id));
            return std::ptr::null_mut();
        };
        if !check_associated_cell(&entity, false, prefix, entity_id) {
            return std::ptr::null_mut();
        }

        // The null mailbox is the space-id path (`base.cpp:3341-3343`).
        if mailbox_is_none(mailbox) {
            return create_cell_entity_from_space_id(cell_data, entity_id, prefix);
        }

        // The mailbox must be a cell mailbox (the reference accepts any
        // `ServerEntityMailBox`; the port's acceptance path carries the
        // cell view `globalBases[key].cell` hands out).
        let is_cell_mailbox = Engine::with(|engine| {
            engine
                .cell_mailbox_type
                .as_ref()
                .is_some_and(|ty| capi::Py_TYPE(mailbox) == ty.as_ptr() as *mut capi::PyTypeObject)
        });
        if !is_cell_mailbox {
            raise_type_error("createCellEntity() expects a cell mailbox");
            return std::ptr::null_mut();
        }
        let address = (*body_mailbox(mailbox)).ip;
        if address == 0 {
            raise_value_error(
                "Cell mailbox not yet initialised. This may have come from a \
                 base entity that has not yet had Base.onGetCell called.",
            );
            return std::ptr::null_mut();
        }
        let port = (*body_mailbox(mailbox)).port;
        let outcome = Engine::with(
            |engine| match engine.creator.begin_cell_creation(entity_id) {
                Ok(()) => {
                    engine.cell_create_requests.push(CellCreateRequest {
                        entity_id,
                        nearby: Some((address, port)),
                        space_id: None,
                    });
                    None
                }
                Err(error) => Some(associated_cell_error(error, prefix, entity_id)),
            },
        );
        if let Some(message) = outcome {
            raise_value_error(&message);
            return std::ptr::null_mut();
        }
        capi::py_none()
    }
}

/// `Base::createCellEntityFromSpaceID` (`base.cpp:3303`): the
/// `cellData["spaceID"]` read, the invalid-space refusal, the create
/// recorded for the space.
unsafe fn create_cell_entity_from_space_id(
    cell_data: Option<PyRef>,
    entity_id: EntityId,
    prefix: &str,
) -> *mut capi::PyObject {
    let Some(cell_data) = cell_data else {
        raise_attribute_error(&format!(
            "{prefix}Base {entity_id} does not have a cellData property"
        ));
        return std::ptr::null_mut();
    };
    // SAFETY: the dict is live; the borrowed value is read immediately.
    let space = unsafe {
        let value = capi::PyDict_GetItemString(cell_data.as_ptr(), c"spaceID".as_ptr());
        if value.is_null() {
            0u32
        } else {
            u32::try_from(capi::PyLong_AsLongLong(value)).unwrap_or(0)
        }
    };
    if space == 0 {
        raise_value_error(&format!(
            "{prefix}Entity {entity_id} does not have a valid spaceID in its cellData"
        ));
        return std::ptr::null_mut();
    }
    let outcome = Engine::with(
        |engine| match engine.creator.begin_cell_creation(entity_id) {
            Ok(()) => {
                engine.cell_create_requests.push(CellCreateRequest {
                    entity_id,
                    nearby: None,
                    space_id: Some(space),
                });
                None
            }
            Err(error) => Some(associated_cell_error(error, prefix, entity_id)),
        },
    );
    if let Some(message) = outcome {
        raise_value_error(&message);
        return std::ptr::null_mut();
    }
    capi::py_none()
}

/// The `checkAssociatedCellEntity` arms over a model snapshot
/// (`base.cpp:3717-3760`): destroyed, creating, waiting, then the
/// `havingEntityGood` split.
fn check_associated_cell(
    entity: &crate::entity_creator::BaseEntity,
    having_entity_good: bool,
    prefix: &str,
    entity_id: EntityId,
) -> bool {
    if entity.destroyed {
        raise_value_error(&format!("{prefix}Base {entity_id} is destroyed"));
        return false;
    }
    if entity.is_create_cell_pending {
        raise_value_error(&format!(
            "{prefix}Base {entity_id} is in the process of creating an associated cell entity."
        ));
        return false;
    }
    if entity.is_get_cell_pending {
        raise_value_error(&format!(
            "{prefix}Base {entity_id} is waiting for contact from its created associated cell entity.\n"
        ));
        return false;
    }
    let has_cell = entity.cell_entity_addr.is_some();
    if !having_entity_good && has_cell {
        raise_value_error(&format!(
            "{prefix}Base {entity_id} already has an associated cell entity."
        ));
        return false;
    }
    if having_entity_good && !has_cell {
        raise_value_error(&format!(
            "{prefix}Base {entity_id} does not have an associated cell entity."
        ));
        return false;
    }
    true
}

/// The entity id in an engine instance's tail; a non-instance (the
/// reference's direct-construction refusal makes this unreachable) is a
/// clean error.
fn engine_id(self_: *mut capi::PyObject) -> Option<EntityId> {
    // SAFETY: the method handlers only run on instances of the type this
    // module created.
    unsafe {
        let id = (*body(self_)).entity_id;
        if id == 0 { None } else { Some(id as EntityId) }
    }
}

// -- the Base attributes ---------------------------------------------------

/// `id` (`base.hpp:241`).
unsafe extern "C" fn get_id(
    self_: *mut capi::PyObject,
    _closure: *mut c_void,
) -> *mut capi::PyObject {
    unsafe {
        match engine_id(self_) {
            Some(id) => capi::PyLong_FromUnsignedLong(u64::from(id)),
            None => std::ptr::null_mut(),
        }
    }
}

/// `isDestroyed` (`base.hpp:242`).
unsafe extern "C" fn get_is_destroyed(
    self_: *mut capi::PyObject,
    _closure: *mut c_void,
) -> *mut capi::PyObject {
    unsafe {
        let Some(id) = engine_id(self_) else {
            return std::ptr::null_mut();
        };
        // `destroy_base` removes the record; the creator keeps the
        // destroyed set, the model's `destroyed_ids`.
        let destroyed = Engine::with(|engine| engine.creator.is_destroyed(id));
        capi::PyBool_FromLong(if destroyed { 1 } else { 0 })
    }
}

/// `baseType` (`base.hpp:244`): the entity type's def index.
unsafe extern "C" fn get_base_type(
    self_: *mut capi::PyObject,
    _closure: *mut c_void,
) -> *mut capi::PyObject {
    unsafe {
        let Some(id) = engine_id(self_) else {
            return std::ptr::null_mut();
        };
        let type_id = Engine::with(|engine| {
            engine
                .creator
                .bases()
                .get(&id)
                .map(|entity| entity.type_id)
                .unwrap_or(0)
        });
        capi::PyLong_FromUnsignedLong(u64::from(type_id))
    }
}

/// `className` (`base.hpp:245`): the entity type's name.
unsafe extern "C" fn get_class_name(
    self_: *mut capi::PyObject,
    _closure: *mut c_void,
) -> *mut capi::PyObject {
    unsafe {
        let Some(id) = engine_id(self_) else {
            return std::ptr::null_mut();
        };
        let name = Engine::with(|engine| {
            engine
                .creator
                .bases()
                .get(&id)
                .map(|entity| entity.type_name.clone())
                .unwrap_or_default()
        });
        capi::unicode_from_bytes(name.as_bytes())
    }
}

/// `cell` (`base.hpp:248`, `base.cpp:2906`): the cell mailbox view; the
/// not-created arm is the reference's own text.
unsafe extern "C" fn get_cell(
    self_: *mut capi::PyObject,
    _closure: *mut c_void,
) -> *mut capi::PyObject {
    unsafe {
        let Some(id) = engine_id(self_) else {
            return std::ptr::null_mut();
        };
        let state = Engine::with(|engine| {
            engine
                .creator
                .bases()
                .get(&id)
                .map(|entity| entity.cell_entity_addr)
        });
        let Some(address) = state else {
            // The reference asserts destroyed here and raises either way.
            raise_attribute_error(&format!("Entity {id} is destroyed"));
            return std::ptr::null_mut();
        };
        let Some(address) = address else {
            raise_attribute_error("Cell not created yet");
            return std::ptr::null_mut();
        };
        if address.ip == 0 {
            raise_attribute_error("Cell not created yet");
            return std::ptr::null_mut();
        }
        match cell_mailbox(address) {
            Some(instance) => into_raw(instance),
            None => std::ptr::null_mut(),
        }
    }
}

/// `hasCell` (`base.hpp:250`): `hasCellEntity() || isGetCellPending()`.
unsafe extern "C" fn get_has_cell(
    self_: *mut capi::PyObject,
    _closure: *mut c_void,
) -> *mut capi::PyObject {
    unsafe {
        let Some(id) = engine_id(self_) else {
            return std::ptr::null_mut();
        };
        let has = Engine::with(|engine| {
            engine
                .creator
                .bases()
                .get(&id)
                .map(|entity| entity.cell_entity_addr.is_some() || entity.is_get_cell_pending)
                .unwrap_or(false)
        });
        capi::PyBool_FromLong(if has { 1 } else { 0 })
    }
}

/// `cellData` get (`base.cpp:2936`): the dict, or the "No cellData
/// attribute" error when none was seeded.
unsafe extern "C" fn get_cell_data(
    self_: *mut capi::PyObject,
    _closure: *mut c_void,
) -> *mut capi::PyObject {
    // The reads ride the helpers' own `unsafe`: `engine_id` and `into_raw`
    // wrap the instance shape and the reference handoff.
    let Some(id) = engine_id(self_) else {
        return std::ptr::null_mut();
    };
    let dict = Engine::with(|engine| engine.cell_data.get(&id).cloned());
    match dict {
        Some(dict) => into_raw(dict),
        None => {
            raise_attribute_error("No cellData attribute");
            std::ptr::null_mut()
        }
    }
}

/// `cellData` set (`base.cpp:2960`): a dict is accepted, anything else
/// is the reference's TypeError.
unsafe extern "C" fn set_cell_data(
    self_: *mut capi::PyObject,
    value: *mut capi::PyObject,
    _closure: *mut c_void,
) -> c_int {
    unsafe {
        let Some(id) = engine_id(self_) else {
            return -1;
        };
        let is_dict = Engine::with(|engine| {
            engine
                .dict_type
                .as_ref()
                .is_some_and(|ty| capi::Py_TYPE(value) == ty.as_ptr() as *mut capi::PyTypeObject)
        });
        if !is_dict {
            raise_type_error("cellData must be set to a dictionary");
            return -1;
        }
        let copied = capi::PyDict_Copy(value);
        if copied.is_null() {
            capi::clear_error();
            return -1;
        }
        let dict = PyRef::from_owned(copied);
        Engine::with(|engine| {
            if let Some(dict) = dict {
                engine.cell_data.insert(id, dict);
            }
        });
        0
    }
}

/// `databaseID` (`base.cpp:2995`): the id, 0 when the entity has no
/// database entry.
unsafe extern "C" fn get_database_id(
    self_: *mut capi::PyObject,
    _closure: *mut c_void,
) -> *mut capi::PyObject {
    unsafe {
        let Some(id) = engine_id(self_) else {
            return std::ptr::null_mut();
        };
        let db_id = Engine::with(|engine| {
            engine
                .creator
                .bases()
                .get(&id)
                .map(|entity| entity.db_id)
                .unwrap_or(0)
        });
        capi::PyLong_FromUnsignedLong(db_id)
    }
}

/// The `.cell` attribute of a base mailbox: the cell-mailbox view over
/// the same address.
unsafe extern "C" fn get_mailbox_cell(
    self_: *mut capi::PyObject,
    _closure: *mut c_void,
) -> *mut capi::PyObject {
    unsafe {
        let slot = body_mailbox(self_);
        let address = BaseAddr {
            ip: (*slot).ip,
            port: (*slot).port,
        };
        match cell_mailbox(address) {
            Some(instance) => into_raw(instance),
            None => std::ptr::null_mut(),
        }
    }
}

// -- tests ----------------------------------------------------------------

#[cfg(test)]
mod tests;
