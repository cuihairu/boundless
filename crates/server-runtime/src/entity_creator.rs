//! BW-07-01-01 dynamically creating a Base entity and allocating its id.
//!
//! `EntityCreator::createBaseLocally` / `createBase`
//! (`server/baseapp/entity_creator.cpp`) implement the script `createBase`:
//!
//! 1. the first argument names the entity type, which must be on the base and
//!    not a service;
//! 2. the remaining arguments are split into base and cell data by each
//!    property's `isBaseData`/`isCellData` flags. A property provided in the
//!    args moves to the cell data when it is cell data; a property not provided
//!    is filled with its initial value. `position`, `direction` and `spaceID`
//!    move to the cell data (unless the def marks them base data);
//! 3. `createBase` allocates an id from the `IDClient` pool (`id_client.cpp` —
//!    note that `MF_ID_RECYCLING` is off in this build, so `putUsedID` is a
//!    no-op), constructs the entity via `newEntityBase`, and `Base::init`
//!    (`base.cpp`) assigns the base properties, stores the cell data and calls
//!    the script `__init__` — the creation callback.
//!
//! The recorded callbacks are the acceptance: 类型、初始化参数和回调吻合. The
//! remote `createBase*` paths (which carry an explicit callback over the wire)
//! are a later item; this models the local script path.
//!
//! # BW-07-01-02 按 DBID 装载 Base
//!
//! `EntityCreator::createBaseFromDB` (`entity_creator.cpp`) loads a base by its
//! database id: the type is validated (services excluded), an entity id is
//! allocated up front and a `loadEntity` request goes to the DBApp. The reply
//! is handled by `LoadEntityHandler::handleMessage`
//! (`server/baseapp/load_entity_handler.cpp`, the DBApp side is
//! `server/dbapp/load_entity_handler.cpp`) and is one of:
//!
//! * `LOGGED_ON` + dbID + the persisted data — the record existed. The base
//!   section (`EntityDescription::readStreamToDict` with
//!   `ONLY_PERSISTENT_DATA`) is parsed strictly: non-persistent base
//!   properties are filled from their initial values
//!   (`insertDefaultProperties`) and a truncated stream fails the build. The
//!   cell section is captured whole by `PyCellData` and parsed only when used,
//!   so the model reads it leniently — a bad cell section never fails the
//!   load. A build calls `Base::init` (`__init__` then
//!   `onEntityInitialised`) and the callback gets
//!   `(base, dbID, wasActive = false)`;
//! * `LOGIN_REJECTED_ALREADY_LOGGED_IN` + dbID + a mailbox — the record is
//!   already checked out elsewhere. The id goes back via `putUsedID` (a no-op
//!   while recycling is off) and the callback gets `(mailbox, dbID, true)`;
//! * a failure (`sendFailure`, e.g. `LOGIN_REJECTED_NO_SUCH_USER` for a
//!   missing record) or a channel exception — the callback gets
//!   `(None, 0, false)`.
//!
//! A base that fails to build from an existing record is written back with
//! `unCheckoutEntity` (`writeEntity` + `WRITE_LOG_OFF`) so the record can be
//! loaded again; the callback still fires once, with `(None, dbID, false)`.
//!
//! The persisted stream is modelled with a compact stand-in for the per-type
//! `DataDescription::createFromStream` encoding: values appear in definition
//! order per domain, typed by the property's initial value — `i64`/`f64`
//! little-endian, a `BW::string` packed-int length plus bytes, one bool byte,
//! three `f32` little-endian.
//!
//! # BW-07-01-04 销毁驻留实体并归还 ID
//!
//! `Base::destroy(deleteFromDB, writeToDB, logOffFromDB)` (`base.cpp`)
//! destroys a base and returns its id:
//!
//! 1. an already-destroyed base (`isDestroyed_`) or a re-entrant destroy
//!    (`inDestroy_`) returns immediately — the acceptance, a repeated destroy
//!    is a no-op and never recycles the id again;
//! 2. `inDestroy_` is set, the script `onDestroy` is called and the
//!    keep-alive timer is cancelled (not modelled yet);
//! 3. a base that has written to the database (`hasWrittenToDB()` =
//!    `databaseID_ != 0`) writes the flags back — `WRITE_LOG_OFF` when logging
//!    off, plus `WRITE_DELETE_FROM_DB` when deleting, or `WRITE_BASE_CELL_DATA`
//!    when only writing;
//! 4. the global bases are told (`onBaseDestroyed`) and `discard` removes the
//!    base, returns the id via `putUsedID` (a no-op while recycling is off, but
//!    the call is recorded here) and clears the entity.
//!
//! The script path (`py_destroy`) passes `writeToDB = hasWrittenToDB()` and the
//! default `logOffFromDB = true`.
//!
//! # BW-07-01-05 处理创建及装载失败
//!
//! The create and load failures keep the model consistent and retryable:
//!
//! * a local `createBase` failure (invalid type, empty id pool) raises a
//!   Python exception and fires no callback — the synchronous path has no
//!   callback — and leaves no base and no consumed id (`createBase` allocates
//!   the id only after the type is resolved and `newEntityBase` succeeds);
//! * `createBaseFromDB` validates the database id and the type before it
//!   allocates the id, so a validation failure consumes nothing; a failure to
//!   allocate consumes nothing either;
//! * a load reply that fails (`sendFailure`) or a channel exception
//!   (`handleException`) fires the callback exactly once with
//!   `(None, 0, false)` and returns the id via `putUsedID` (`entity_creator.cpp`
//!   and `load_entity_handler.cpp`) — the request is consumed, so a retry is a
//!   fresh `createBaseFromDB` that allocates a new id (recycling is off, so a
//!   failed load's id is never reused);
//! * a base that fails to build from an existing record fires the callback once
//!   with `(None, dbID, false)` and writes the record back (`unCheckoutEntity`).
//!
//! # BW-07-02-02 处理 Cell 创建成功与失败
//!
//! `Base::createCellEntity` (`baseapp/base.cpp:3330`) requests the associated
//! cell entity and the reply decides the outcome:
//!
//! * `prepareForCellCreate` (`base.cpp:3269`) validates the base (not
//!   destroyed, no cell create or contact pending, no cell entity yet, the
//!   type is on the cell) and sets `isCreateCellPending_` and
//!   `isGetCellPending_`;
//! * `CreateCellEntityHandler::handleMessage`
//!   (`baseapp/create_cell_entity_handler.cpp:31`) reads the created entity
//!   id; when it is this base's id the create succeeded, otherwise it failed;
//! * `handleException` (a nub failure or timeout) fails the create;
//! * `Base::cellCreationResult` (`base.cpp:3759`) clears
//!   `isCreateCellPending_`; on failure it also clears `isGetCellPending_` and
//!   calls the script `onCreateCellFailure`. A destroyed base, or a reply with
//!   no create pending (the rare race after `setCurrentCell`), is ignored;
//! * once the cell entity contacts the base, `setCurrentCell`-style handling
//!   (`base.cpp:995`) records the cell address and space, clears
//!   `isGetCellPending_`, consumes the cell data and calls the script
//!   `onGetCell`.
//!
//! The recorded callbacks and the pending/address state are the acceptance:
//! 回调及 Base 状态一致.
//!
//! # BW-07-02-03 处理 Cell 丢失通知
//!
//! `Base::setCurrentCell( spaceID, NONE )` (`base.cpp:954`, the `!haveCell`
//! branch) retires the old cell reference: the cell mailbox address becomes
//! `NONE` and the space is updated unconditionally, and when the base had a
//! cell (`hadCell != haveCell`) the contact pending clears, the cell backup
//! data drops (`cellBackupData_.clear()` — the backup model itself is
//! BW-07-04), a proxy base is told `cellEntityDestroyed` first, and the
//! script `onLoseCell` is called. A base with no cell has no transition and
//! records nothing. The acceptance: Base 清退旧 Cell 引用.

use std::collections::HashMap;
use std::collections::HashSet;
use std::collections::VecDeque;

use crate::baseappmgr::Addr;
use crate::cell_data::BaseAddr;
use crate::mailbox_call::{MailboxCallError, MailboxMethodCall, value_matches};

/// `EntityID`.
pub type EntityId = u32;

/// `DatabaseID`.
pub type DatabaseId = u64;

/// A property value (the Python object model is reduced to these).
#[derive(Clone, Debug, PartialEq)]
pub enum PropValue {
    /// An integer.
    Int(i64),
    /// A float.
    Float(f64),
    /// A string.
    Str(String),
    /// A boolean.
    Bool(bool),
    /// A `Vector3`.
    Vec3([f32; 3]),
}

/// One property of an entity type (`DataDescription`-lite).
#[derive(Clone, Debug, PartialEq)]
pub struct PropDesc {
    /// The property name.
    pub name: String,
    /// `isBaseData()`.
    pub base_data: bool,
    /// `isCellData()`.
    pub cell_data: bool,
    /// `isComponentised()`.
    pub componentised: bool,
    /// `isPersistent()`: a non-persistent property is not in the persisted
    /// stream and is filled from its initial value on a load.
    pub persistent: bool,
    /// `pInitialValue()`.
    pub initial: PropValue,
}

impl PropDesc {
    /// Constructor.
    pub fn new(
        name: &str,
        base_data: bool,
        cell_data: bool,
        componentised: bool,
        persistent: bool,
        initial: PropValue,
    ) -> Self {
        Self {
            name: name.to_string(),
            base_data,
            cell_data,
            componentised,
            persistent,
            initial,
        }
    }
}

/// `MethodDescription::Component` (`method_description.hpp:60`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MethodComponent {
    /// `CLIENT`: a method the server calls on the owning client.
    Client,
    /// `CELL`: a method on the cell entity.
    Cell,
    /// `BASE`: a method on the base entity.
    Base,
}

/// One method of an entity type (`MethodDescription`-lite): the wire index
/// plus the argument and return-value type witnesses, which is what the
/// mailbox codec needs to stream and parse a call and its reply.
#[derive(Clone, Debug, PartialEq)]
pub struct MethodDesc {
    /// The method name.
    pub name: String,
    /// `MethodDescription::internalIndex()`: the index a call carries.
    pub internal_index: u32,
    /// `MethodDescription::component()`: where the method lives.
    pub component: MethodComponent,
    /// `MethodDescription::isExposed()`: a client-exposed method.
    pub exposed: bool,
    /// One type witness per argument, in stream order.
    pub arg_kinds: Vec<PropValue>,
    /// One type witness per return value, in stream order.
    pub return_kinds: Vec<PropValue>,
}

impl MethodDesc {
    /// Build a method description.
    pub fn new(
        name: &str,
        internal_index: u32,
        component: MethodComponent,
        exposed: bool,
        arg_kinds: Vec<PropValue>,
        return_kinds: Vec<PropValue>,
    ) -> Self {
        Self {
            name: name.to_string(),
            internal_index,
            component,
            exposed,
            arg_kinds,
            return_kinds,
        }
    }

    /// `MethodDescription::hasReturnValues()`.
    pub fn has_return_values(&self) -> bool {
        !self.return_kinds.is_empty()
    }
}

/// An entity type (`EntityType`-lite).
#[derive(Clone, Debug, PartialEq)]
pub struct EntityType {
    /// The type id.
    pub id: u32,
    /// The type name.
    pub name: String,
    /// `canBeOnBase()`.
    pub can_be_on_base: bool,
    /// `isService()`.
    pub is_service: bool,
    /// `isProxy()`.
    pub is_proxy: bool,
    /// `canBeOnCell()`.
    pub can_be_on_cell: bool,
    /// The properties.
    pub properties: Vec<PropDesc>,
    /// The methods (`MethodDescription`s), in definition order.
    pub methods: Vec<MethodDesc>,
}

impl EntityType {
    /// Find a property by name.
    pub fn property(&self, name: &str) -> Option<&PropDesc> {
        self.properties.iter().find(|p| p.name == name)
    }

    /// Find a method by its wire index (`MethodDescription` lookup by
    /// `internalIndex`).
    pub fn method(&self, internal_index: u32) -> Option<&MethodDesc> {
        self.methods
            .iter()
            .find(|m| m.internal_index == internal_index)
    }
}

/// The `IDClient` id pool (`id_client.cpp`).
#[derive(Debug)]
pub struct IdPool {
    ready: VecDeque<EntityId>,
    low_size: usize,
    desired_size: usize,
    high_size: usize,
    critically_low_size: usize,
    pending_request: bool,
    in_emergency: bool,
}

impl IdPool {
    /// Constructor with the size thresholds (`criticallyLow < low < desired <
    /// high`).
    pub fn new(critically_low: usize, low: usize, desired: usize, high: usize) -> Self {
        Self {
            ready: VecDeque::new(),
            low_size: low,
            desired_size: desired,
            high_size: high,
            critically_low_size: critically_low,
            pending_request: false,
            in_emergency: true,
        }
    }

    /// `getID`: pop an id. When the pool is empty, an emergency refill is
    /// requested; if none arrives, `None` (the reference returns 0).
    pub fn get_id(&mut self) -> Option<EntityId> {
        if self.ready.is_empty() {
            self.perform_updates(true);
            if self.ready.is_empty() {
                return None;
            }
        }
        let id = self.ready.pop_front().unwrap();
        self.perform_updates(false);
        Some(id)
    }

    /// The number of ids ready to hand out.
    pub fn len(&self) -> usize {
        self.ready.len()
    }

    /// Whether no ids are ready.
    pub fn is_empty(&self) -> bool {
        self.ready.is_empty()
    }

    /// Whether the pool has asked the upstream for more ids.
    pub fn pending_request(&self) -> bool {
        self.pending_request
    }

    /// `retrieveIDsFromStream`: the upstream's reply fills the pool.
    pub fn refill(&mut self, ids: &[EntityId]) {
        self.ready.extend(ids.iter().copied());
        self.pending_request = false;
    }

    /// `putUsedID`: a no-op while `MF_ID_RECYCLING` is off.
    pub fn put_used_id(&mut self, _id: EntityId) {}

    /// `returnIDs`: hand the ready ids back to the upstream.
    pub fn return_ids(&mut self) -> Vec<EntityId> {
        self.ready.drain(..).collect()
    }

    /// `performUpdates`: request more ids below the low watermark and grow the
    /// thresholds after an emergency.
    fn perform_updates(&mut self, is_emergency: bool) {
        let emergency = is_emergency || self.ready.len() < self.critically_low_size;
        if emergency && !self.in_emergency {
            // Grow the thresholds to avoid a repeat.
            self.high_size *= 2;
            self.desired_size *= 2;
            self.low_size *= 2;
            self.in_emergency = true;
        }
        if self.ready.len() < self.low_size && !self.pending_request {
            self.pending_request = true;
        }
    }
}

/// A callback recorded during creation.
#[derive(Clone, Debug, PartialEq)]
pub enum BaseCallback {
    /// The script `__init__` was called.
    Init {
        /// The entity id.
        entity_id: EntityId,
        /// The type name.
        type_name: String,
    },
    /// `onEntityInitialised` was called.
    Initialised {
        /// The entity id.
        entity_id: EntityId,
    },
    /// `onCreateCellFailure` was called.
    OnCreateCellFailure {
        /// The entity id.
        entity_id: EntityId,
    },
    /// `onGetCell` was called with the cell contact.
    OnGetCell {
        /// The entity id.
        entity_id: EntityId,
        /// The space the cell entity is in.
        space_id: u32,
        /// The cell app address of the cell entity.
        cell_addr: BaseAddr,
    },
    /// `Proxy::cellEntityDestroyed` was called (a proxy base losing its cell).
    CellEntityDestroyed {
        /// The entity id.
        entity_id: EntityId,
    },
    /// `onLoseCell` was called.
    OnLoseCell {
        /// The entity id.
        entity_id: EntityId,
    },
    /// A mailbox method call was delivered to the base
    /// (`ServerEntityMailBox::callMethod` reaching its target).
    MethodCalled {
        /// The entity id.
        entity_id: EntityId,
        /// The method name.
        method_name: String,
        /// The method's internal index.
        method_index: u32,
        /// The source entity id for an exposed CELL method, `None` for
        /// `NULL_ENTITY_ID`.
        source_entity_id: Option<EntityId>,
        /// The delivered arguments.
        args: Vec<PropValue>,
    },
    /// A cell method call was forwarded to the associated cell entity
    /// (`Base::callCellMethod`, `base.cpp:1391`). The cell address is the
    /// base's tracked `Real` location, which after a migration is the new
    /// cellapp.
    CellMethodCalled {
        /// The entity id.
        entity_id: EntityId,
        /// The cell mailbox address the call was routed to.
        cell_addr: BaseAddr,
        /// The method name.
        method_name: String,
        /// The method's internal index.
        method_index: u32,
        /// The source entity id for an exposed CELL method, `None` for
        /// `NULL_ENTITY_ID`.
        source_entity_id: Option<EntityId>,
        /// The forwarded arguments.
        args: Vec<PropValue>,
    },
}

/// A created Base entity.
#[derive(Clone, Debug, PartialEq)]
pub struct BaseEntity {
    /// The entity id.
    pub id: EntityId,
    /// The type id.
    pub type_id: u32,
    /// The type name.
    pub type_name: String,
    /// The database id (0 = not persisted).
    pub db_id: DatabaseId,
    /// Whether this is a proxy.
    pub is_proxy: bool,
    /// `isDestroyed_`: set by `discard`.
    pub destroyed: bool,
    /// `inDestroy_`: set for the duration of a destroy.
    pub in_destroy: bool,
    /// The assigned base properties.
    pub base_props: HashMap<String, PropValue>,
    /// The cell data (defaults plus the cell properties).
    pub cell_props: HashMap<String, PropValue>,
    /// `isCreateCellPending_`: a `createCellEntity` request is outstanding.
    pub is_create_cell_pending: bool,
    /// `isGetCellPending_`: waiting for the created cell entity to contact
    /// the base (`onGetCell`).
    pub is_get_cell_pending: bool,
    /// `hasCellEntity()`: the cell app address of the associated cell
    /// entity, `None` while there is none.
    pub cell_entity_addr: Option<BaseAddr>,
    /// `spaceID_`: the space the associated cell entity is in.
    pub cell_space_id: u32,
    /// The callbacks recorded in order.
    pub callbacks: Vec<BaseCallback>,
}

/// The failure of a `createBase` call.
#[derive(Clone, Debug, PartialEq)]
pub enum CreateError {
    /// The type name is unknown, not on the base or is a service.
    InvalidType { name: String },
    /// The database id must be non-zero (`createBaseFromDB` ValueError).
    InvalidDbId,
    /// The id pool had no id to hand out.
    NoId,
}

/// A `createCellEntity` request rejected by `checkAssociatedCellEntity` /
/// `prepareForCellCreate` (`base.cpp:3718` and `base.cpp:3269`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CellCreateError {
    /// The base is destroyed.
    Destroyed,
    /// The base is already creating an associated cell entity.
    AlreadyCreating,
    /// The base is waiting for contact from its created cell entity.
    WaitingForContact,
    /// The base already has an associated cell entity.
    AlreadyHasCell,
    /// The type has no cell script (`canBeOnCell` is false).
    NoCellScript,
    /// The base is unknown.
    UnknownBase,
}

/// The outcome of a `cellCreationResult` reply.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CellCreationResult {
    /// The create pending cleared; the cell entity will contact the base.
    Created,
    /// Both pendings cleared and `onCreateCellFailure` was recorded.
    Failed,
    /// Ignored: the base is destroyed or no create was pending.
    Ignored,
}

/// The failure of a cell mailbox migration (`Base::currentCell`,
/// `base.cpp:1071`: the associated cell entity moved to a new cellapp).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CellMigrateError {
    /// The base is unknown (a destroyed base is removed and is unknown).
    UnknownBase,
    /// The base has no associated cell entity to move.
    NoCell,
    /// The migration address is `NONE`: a cell is being lost, which is the
    /// `cell_lost` path, not a migration.
    LostCell,
}

/// The reference `LogOnStatus` values this load path answers with
/// (`lib/connection/log_on_status.hpp`).
pub mod load_status {
    /// `LOGGED_ON`.
    pub const LOGGED_ON: u8 = 1;
    /// `LOGIN_REJECTED_NO_SUCH_USER`.
    pub const LOGIN_REJECTED_NO_SUCH_USER: u8 = 67;
    /// `LOGIN_REJECTED_ALREADY_LOGGED_IN`.
    pub const LOGIN_REJECTED_ALREADY_LOGGED_IN: u8 = 69;
    /// `LOGIN_REJECTED_DB_GENERAL_FAILURE`.
    pub const LOGIN_REJECTED_DB_GENERAL_FAILURE: u8 = 71;
}

/// `EntityMailBoxRef::Component` (`lib/network/basictypes.hpp`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MailBoxComponent {
    /// `CELL`.
    Cell,
    /// `BASE`.
    Base,
    /// `CLIENT`.
    Client,
    /// `BASE_VIA_CELL`.
    BaseViaCell,
    /// `CLIENT_VIA_CELL`.
    ClientViaCell,
    /// `CELL_VIA_BASE`.
    CellViaBase,
    /// `CLIENT_VIA_BASE`.
    ClientViaBase,
    /// `SERVICE`.
    Service,
}

/// The mailbox reference of an entity (`EntityMailBoxRef`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EntityMailBoxRef {
    /// The entity's id.
    pub id: EntityId,
    /// The address the entity lives at.
    pub addr: Addr,
    /// The mailbox component.
    pub component: MailBoxComponent,
    /// The entity type id.
    pub type_id: u32,
}

impl EntityMailBoxRef {
    /// `init`: build a reference from the id, address, component and type.
    pub fn init(id: EntityId, addr: Addr, component: MailBoxComponent, type_id: u32) -> Self {
        Self {
            id,
            addr,
            component,
            type_id,
        }
    }
}

/// A handle to a pending `createBaseFromDB` request.
pub type LoadRequestId = usize;

/// The DBApp's reply to a `loadEntity` request.
#[derive(Clone, Debug, PartialEq)]
pub enum LoadReply {
    /// `LOGGED_ON`: the record existed and was checked out to us. `stream` is
    /// the persisted entity data (base section, then the cell section).
    Found {
        /// The record's database id.
        db_id: DatabaseId,
        /// The persisted entity data.
        stream: Vec<u8>,
    },
    /// `LOGIN_REJECTED_ALREADY_LOGGED_IN`: the record is checked out
    /// elsewhere.
    AlreadyActive {
        /// The record's database id.
        db_id: DatabaseId,
        /// Where the entity currently lives.
        mailbox: EntityMailBoxRef,
    },
    /// A `sendFailure` reply (a missing record is
    /// `LOGIN_REJECTED_NO_SUCH_USER` / "No such user.").
    Failed {
        /// The `LogOnStatus` value.
        status: u8,
        /// The failure description.
        description: String,
    },
    /// The request channel failed (`handleException`).
    Exception,
}

/// The argument of the load callback (`onLoadedFromDB`'s `pArg`).
#[derive(Clone, Debug, PartialEq)]
pub enum CallbackArg {
    /// The Base built from the persisted data.
    Loaded(BaseEntity),
    /// The mailbox of the already-checked-out entity.
    MailBox(EntityMailBoxRef),
    /// No entity (`Py_None`).
    None,
}

/// The recorded load callback: `pResultHandler(arg, dbID, wasActive)`.
#[derive(Clone, Debug, PartialEq)]
pub struct LoadCallback {
    /// The entity argument.
    pub arg: CallbackArg,
    /// The record's database id (0 on a failure).
    pub db_id: DatabaseId,
    /// Whether the entity was already active (`pMailbox != NULL`).
    pub was_active: bool,
}

impl LoadCallback {
    /// The created base, if the load built one.
    pub fn entity(&self) -> Option<&BaseEntity> {
        if let CallbackArg::Loaded(base) = &self.arg {
            Some(base)
        } else {
            None
        }
    }

    /// The mailbox of an already-active entity, if there was one.
    pub fn mailbox(&self) -> Option<&EntityMailBoxRef> {
        if let CallbackArg::MailBox(mailbox) = &self.arg {
            Some(mailbox)
        } else {
            None
        }
    }

    /// Whether the callback got no entity at all.
    pub fn is_none(&self) -> bool {
        matches!(self.arg, CallbackArg::None)
    }
}

/// The BaseApp→DBApp `writeEntity` message issued when a base fails to build
/// from an existing record (`unCheckoutEntity`): the record is written back
/// with `WRITE_LOG_OFF` so it can be loaded again.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Uncheckout {
    /// The entity type id.
    pub type_id: u32,
    /// The record's database id.
    pub db_id: DatabaseId,
    /// The entity id allocated for the failed load.
    pub entity_id: EntityId,
}

/// The `WriteDBFlag` values (`lib/server/writedb.hpp`).
pub mod write_db {
    /// `WRITE_LOG_OFF`.
    pub const LOG_OFF: u16 = 1;
    /// `WRITE_BASE_DATA`.
    pub const BASE_DATA: u16 = 4;
    /// `WRITE_CELL_DATA`.
    pub const CELL_DATA: u16 = 8;
    /// `WRITE_BASE_CELL_DATA`.
    pub const BASE_CELL_DATA: u16 = BASE_DATA | CELL_DATA;
    /// `WRITE_DELETE_FROM_DB`.
    pub const DELETE_FROM_DB: u16 = 16;
}

/// A `writeToDB` issued by a destroy (`Base::destroy`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WriteDbEvent {
    /// The entity id.
    pub entity_id: EntityId,
    /// The `WriteDBFlags`.
    pub flags: u16,
}

/// The outcome of [`EntityCreator::destroy_base`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DestroyOutcome {
    /// The base was destroyed and its id returned once.
    Destroyed {
        /// The destroyed entity's id.
        id: EntityId,
    },
    /// The base was already destroyed or is mid-destroy; a no-op.
    AlreadyDestroyed {
        /// The entity's id.
        id: EntityId,
    },
    /// No base with this id is alive.
    NotFound {
        /// The entity's id.
        id: EntityId,
    },
}

/// A pending `createBaseFromDB` request (`LoadEntityHandler`'s state).
#[derive(Clone, Debug)]
struct PendingLoad {
    entity_id: EntityId,
    type_id: u32,
    type_name: String,
}

/// `EntityCreator`: the type registry, the id pool, the created bases and the
/// pending `createBaseFromDB` requests.
#[derive(Debug)]
pub struct EntityCreator {
    types: HashMap<String, EntityType>,
    id_pool: IdPool,
    bases: HashMap<EntityId, BaseEntity>,
    pending_loads: HashMap<LoadRequestId, PendingLoad>,
    next_load_id: LoadRequestId,
    uncheckouts: Vec<Uncheckout>,
    destroyed_ids: HashSet<EntityId>,
    destroy_calls: Vec<EntityId>,
    base_destroyed_calls: Vec<EntityId>,
    write_db: Vec<WriteDbEvent>,
    returned_ids: Vec<EntityId>,
}

/// The `createBase` script arguments: the type name and the init values.
pub type CreateArgs = HashMap<String, PropValue>;

impl EntityCreator {
    /// Constructor.
    pub fn new(id_pool: IdPool) -> Self {
        Self {
            types: HashMap::new(),
            id_pool,
            bases: HashMap::new(),
            pending_loads: HashMap::new(),
            next_load_id: 0,
            uncheckouts: Vec::new(),
            destroyed_ids: HashSet::new(),
            destroy_calls: Vec::new(),
            base_destroyed_calls: Vec::new(),
            write_db: Vec::new(),
            returned_ids: Vec::new(),
        }
    }

    /// Register an entity type.
    pub fn add_type(&mut self, ty: EntityType) {
        self.types.insert(ty.name.clone(), ty);
    }

    /// The id pool.
    pub fn id_pool(&self) -> &IdPool {
        &self.id_pool
    }

    /// The created bases.
    pub fn bases(&self) -> &HashMap<EntityId, BaseEntity> {
        &self.bases
    }

    /// `createBaseLocally`: consolidate the args and create the base.
    pub fn create_base_locally(
        &mut self,
        type_name: &str,
        args: CreateArgs,
    ) -> Result<BaseEntity, CreateError> {
        let ty = self
            .types
            .get(type_name)
            .ok_or_else(|| CreateError::InvalidType {
                name: type_name.to_string(),
            })?;
        if !ty.can_be_on_base || ty.is_service {
            return Err(CreateError::InvalidType {
                name: type_name.to_string(),
            });
        }

        let mut base_args = args;
        let mut cell_args = HashMap::new();

        // The default spatial data when the entity can be on a cell.
        if ty.can_be_on_cell {
            cell_args.insert("position".to_string(), PropValue::Vec3([0.0; 3]));
            cell_args.insert("direction".to_string(), PropValue::Vec3([0.0; 3]));
            cell_args.insert("spaceID".to_string(), PropValue::Int(0));
        }

        // Split each property by its flags: a cell-data property always goes to
        // the cell data (and leaves the base args), a base-only property stays
        // in the base args. A provided value wins, otherwise the initial value.
        for prop in &ty.properties {
            if !prop.base_data && !prop.cell_data {
                continue;
            }
            let value = base_args
                .remove(&prop.name)
                .unwrap_or_else(|| prop.initial.clone());
            if prop.cell_data {
                if ty.can_be_on_cell {
                    cell_args.insert(prop.name.clone(), value);
                }
            } else if prop.base_data {
                base_args.insert(prop.name.clone(), value);
            }
        }

        // Move position/direction/spaceID to the cell data.
        for attr in ["position", "direction", "spaceID"] {
            if let Some(value) = base_args.remove(attr) {
                if ty.can_be_on_cell {
                    cell_args.insert(attr.to_string(), value.clone());
                }
                // A base-data property named the same stays in the base args.
                let is_base_data = ty.property(attr).map(|p| p.base_data).unwrap_or(false);
                if is_base_data {
                    base_args.insert(attr.to_string(), value);
                }
            }
        }

        // The type snapshot: the base creation needs the id, name and proxy
        // flag, owned so the mutable call below can proceed.
        let (type_id, type_name, is_proxy) = (ty.id, ty.name.clone(), ty.is_proxy);
        self.create_base(type_id, &type_name, is_proxy, base_args, cell_args)
    }

    /// `ServiceStarter::run` → `EntityCreator::createBase`
    /// (`service_starter.cpp:222`): a ServiceApp starts one instance of each
    /// service entity type declared in the entity defs. Unlike a login
    /// `createBaseLocally`, a service entity is created directly, so the
    /// `is_service` check that guards logins does not apply here.
    pub fn create_service(&mut self, type_name: &str) -> Result<BaseEntity, CreateError> {
        let ty = self
            .types
            .get(type_name)
            .ok_or_else(|| CreateError::InvalidType {
                name: type_name.to_string(),
            })?;
        if !ty.is_service {
            return Err(CreateError::InvalidType {
                name: type_name.to_string(),
            });
        }
        let (type_id, type_name, is_proxy) = (ty.id, ty.name.clone(), ty.is_proxy);
        self.create_base(
            type_id,
            &type_name,
            is_proxy,
            HashMap::new(),
            HashMap::new(),
        )
    }

    /// `createBase`: allocate an id, build the entity and initialise it.
    fn create_base(
        &mut self,
        type_id: u32,
        type_name: &str,
        is_proxy: bool,
        base_args: HashMap<String, PropValue>,
        cell_args: HashMap<String, PropValue>,
    ) -> Result<BaseEntity, CreateError> {
        let id = self.id_pool.get_id().ok_or(CreateError::NoId)?;

        let mut entity = BaseEntity {
            id,
            type_id,
            type_name: type_name.to_string(),
            db_id: 0,
            is_proxy,
            destroyed: false,
            in_destroy: false,
            base_props: HashMap::new(),
            cell_props: HashMap::new(),
            is_create_cell_pending: false,
            is_get_cell_pending: false,
            cell_entity_addr: None,
            cell_space_id: 0,
            callbacks: Vec::new(),
        };

        // `Base::init`: assign the base properties, store the cell data, then
        // call the script `__init__` and `onEntityInitialised`.
        for (name, value) in base_args {
            entity.base_props.insert(name, value);
        }
        entity.cell_props = cell_args;
        entity.callbacks.push(BaseCallback::Init {
            entity_id: id,
            type_name: type_name.to_string(),
        });
        entity
            .callbacks
            .push(BaseCallback::Initialised { entity_id: id });

        self.bases.insert(id, entity.clone());
        Ok(entity)
    }

    /// `Base::prepareForCellCreate` (`base.cpp:3269`): validate the base is
    /// eligible for an associated cell entity, then set the create and
    /// contact pendings.
    pub fn begin_cell_creation(&mut self, entity_id: EntityId) -> Result<(), CellCreateError> {
        let entity = self
            .bases
            .get_mut(&entity_id)
            .ok_or(CellCreateError::UnknownBase)?;
        if entity.destroyed {
            return Err(CellCreateError::Destroyed);
        }
        if entity.is_create_cell_pending {
            return Err(CellCreateError::AlreadyCreating);
        }
        if entity.is_get_cell_pending {
            return Err(CellCreateError::WaitingForContact);
        }
        if entity.cell_entity_addr.is_some() {
            return Err(CellCreateError::AlreadyHasCell);
        }
        let ty = self
            .types
            .get(&entity.type_name)
            .ok_or(CellCreateError::NoCellScript)?;
        if !ty.can_be_on_cell {
            return Err(CellCreateError::NoCellScript);
        }
        entity.is_create_cell_pending = true;
        entity.is_get_cell_pending = true;
        Ok(())
    }

    /// `Base::cellCreationResult` (`base.cpp:3759`): the outcome of the
    /// create request reply. A destroyed base, or a reply with no create
    /// pending (the rare race after `setCurrentCell`), is ignored. On
    /// failure the contact pending is cleared and `onCreateCellFailure` is
    /// recorded; on success the contact pending stays until the cell entity
    /// contacts the base.
    pub fn cell_creation_result(
        &mut self,
        entity_id: EntityId,
        success: bool,
    ) -> CellCreationResult {
        let Some(entity) = self.bases.get_mut(&entity_id) else {
            return CellCreationResult::Ignored;
        };
        if entity.destroyed || !entity.is_create_cell_pending {
            return CellCreationResult::Ignored;
        }
        entity.is_create_cell_pending = false;
        if success {
            CellCreationResult::Created
        } else {
            entity.is_get_cell_pending = false;
            entity
                .callbacks
                .push(BaseCallback::OnCreateCellFailure { entity_id });
            CellCreationResult::Failed
        }
    }

    /// The `setCurrentCell`-style contact (`base.cpp:995`, the `haveCell`
    /// branch): the cell entity has been created and now contacts the base.
    /// The cell address and space are recorded, the contact pending clears,
    /// the cell data is consumed and `onGetCell` is recorded. Returns false
    /// for an unknown or destroyed base.
    pub fn cell_contact(
        &mut self,
        entity_id: EntityId,
        space_id: u32,
        cell_addr: BaseAddr,
    ) -> bool {
        let Some(entity) = self.bases.get_mut(&entity_id) else {
            return false;
        };
        if entity.destroyed {
            return false;
        }
        entity.cell_entity_addr = Some(cell_addr);
        entity.cell_space_id = space_id;
        entity.is_get_cell_pending = false;
        entity.is_create_cell_pending = false;
        entity.cell_props.clear();
        entity.callbacks.push(BaseCallback::OnGetCell {
            entity_id,
            space_id,
            cell_addr,
        });
        true
    }

    /// `Base::setCurrentCell( spaceID, NONE )` (`base.cpp:954`, the
    /// `!haveCell` branch): the cell entity is gone. The old cell reference
    /// is cleared and the space updated; when the base had a cell the contact
    /// pending clears, a proxy base is told its cell entity was destroyed and
    /// `onLoseCell` is recorded. Returns false for an unknown or destroyed
    /// base.
    pub fn cell_lost(&mut self, entity_id: EntityId, space_id: u32) -> bool {
        let Some(entity) = self.bases.get_mut(&entity_id) else {
            return false;
        };
        if entity.destroyed {
            return false;
        }
        let had_cell = entity.cell_entity_addr.is_some();
        // The mailbox address and the space are set unconditionally.
        entity.cell_entity_addr = None;
        entity.cell_space_id = space_id;
        if !had_cell {
            // No transition: the base had no cell and has none now.
            return true;
        }
        entity.is_get_cell_pending = false;
        if entity.is_proxy {
            entity
                .callbacks
                .push(BaseCallback::CellEntityDestroyed { entity_id });
        }
        entity
            .callbacks
            .push(BaseCallback::OnLoseCell { entity_id });
        true
    }

    /// `ServerEntityMailBox::callMethod` reaching its target: deliver a
    /// decoded [`MailboxMethodCall`] to the base the mailbox addresses.
    ///
    /// A base-addressed mailbox (`BASE`, `SERVICE` or `BASE_VIA_CELL`) is
    /// resolved against the tracked bases; the method must live on the base
    /// component. A cell-addressed mailbox (`CELL` or `CELL_VIA_BASE`) is
    /// forwarded to the associated cell entity at the base's tracked cell
    /// address (`Base::callCellMethod`, `base.cpp:1391`); the method must
    /// live on the cell component and the base must have a cell. A
    /// client-addressed mailbox is outside the base-side model and is
    /// rejected. The call is recorded as a [`BaseCallback::MethodCalled`] (or
    /// [`BaseCallback::CellMethodCalled`]) and the resolved description
    /// returned so the caller can build the reply
    /// ([`MethodReply`](crate::mailbox_call::MethodReply)).
    pub fn deliver_mailbox_call(
        &mut self,
        mailbox: &EntityMailBoxRef,
        call: MailboxMethodCall,
    ) -> Result<MethodDesc, MailboxCallError> {
        let is_base_target = matches!(
            mailbox.component,
            MailBoxComponent::Base | MailBoxComponent::Service | MailBoxComponent::BaseViaCell
        );
        let is_cell_target = matches!(
            mailbox.component,
            MailBoxComponent::Cell | MailBoxComponent::CellViaBase
        );
        if !is_base_target && !is_cell_target {
            return Err(MailboxCallError::ClientTarget);
        }
        let base = self
            .bases
            .get(&call.target_id)
            .ok_or(MailboxCallError::UnknownBase)?;
        let ty = self
            .types
            .get(&base.type_name)
            .ok_or(MailboxCallError::UnknownBase)?;
        let method = ty
            .method(call.method_index)
            .ok_or(MailboxCallError::UnknownMethod)?;
        let expected = if is_cell_target {
            MethodComponent::Cell
        } else {
            MethodComponent::Base
        };
        if method.component != expected {
            return Err(MailboxCallError::WrongComponent);
        }
        if call.args.len() != method.arg_kinds.len() {
            return Err(MailboxCallError::BadArgs {
                expected: method.arg_kinds.len(),
                got: call.args.len(),
            });
        }
        for (index, (value, kind)) in call.args.iter().zip(&method.arg_kinds).enumerate() {
            if !value_matches(value, kind) {
                return Err(MailboxCallError::BadArgKind { index });
            }
        }
        let resolved = method.clone();
        let method_name = method.name.clone();
        let entity_id = call.target_id;
        let source_entity_id = call.source_entity_id;
        let args = call.args;
        if is_cell_target {
            let cell_addr = base.cell_entity_addr.ok_or(MailboxCallError::NoCell)?;
            self.bases
                .get_mut(&entity_id)
                .expect("base still present")
                .callbacks
                .push(BaseCallback::CellMethodCalled {
                    entity_id,
                    cell_addr,
                    method_name,
                    method_index: call.method_index,
                    source_entity_id,
                    args,
                });
        } else {
            self.bases
                .get_mut(&entity_id)
                .expect("base still present")
                .callbacks
                .push(BaseCallback::MethodCalled {
                    entity_id,
                    method_name,
                    method_index: call.method_index,
                    source_entity_id,
                    args,
                });
        }
        Ok(resolved)
    }

    /// `Base::currentCell` (`base.cpp:1071`): the associated cell entity
    /// migrated to a new cellapp (a cell offload). The tracked cell mailbox
    /// address is switched to the new `Real`'s app and the space updated.
    ///
    /// The `hadCell == haveCell` branch of `setCurrentCell`
    /// (`base.cpp:977-1004`): the cell presence is unchanged, so no script
    /// callback is recorded — the mailbox simply points at the new app, so
    /// later calls reach the new `Real`. A `NONE` address is the `cell_lost`
    /// path, not a migration.
    pub fn update_cell_address(
        &mut self,
        entity_id: EntityId,
        space_id: u32,
        new_addr: BaseAddr,
    ) -> Result<(), CellMigrateError> {
        if new_addr.ip == 0 {
            return Err(CellMigrateError::LostCell);
        }
        let entity = self
            .bases
            .get_mut(&entity_id)
            .ok_or(CellMigrateError::UnknownBase)?;
        if entity.cell_entity_addr.is_none() {
            return Err(CellMigrateError::NoCell);
        }
        entity.cell_entity_addr = Some(new_addr);
        entity.cell_space_id = space_id;
        Ok(())
    }

    /// The cell mailbox ref for the base's associated cell entity
    /// (`Base::cellEntityMailBox`, `base.cpp:2915`): component `CELL`, the
    /// tracked cell address and the base's id and type. `None` while the
    /// base has no cell.
    pub fn cell_mailbox(&self, entity_id: EntityId) -> Option<EntityMailBoxRef> {
        let base = self.bases.get(&entity_id)?;
        let cell_addr = base.cell_entity_addr?;
        Some(EntityMailBoxRef {
            id: base.id,
            addr: (cell_addr.ip, cell_addr.port),
            component: MailBoxComponent::Cell,
            type_id: base.type_id,
        })
    }

    /// `createBaseFromDB`: validate the type and the database id, allocate an
    /// entity id and register the pending `loadEntity` request.
    ///
    /// The order matches the reference: a zero database id is rejected first,
    /// then the type (services excluded), then the id allocation. On success
    /// the returned handle is passed to [`on_load_reply`].
    ///
    /// [`on_load_reply`]: EntityCreator::on_load_reply
    pub fn create_base_from_db(
        &mut self,
        type_name: &str,
        db_id: DatabaseId,
    ) -> Result<LoadRequestId, CreateError> {
        if db_id == 0 {
            return Err(CreateError::InvalidDbId);
        }
        let ty = self
            .types
            .get(type_name)
            .ok_or_else(|| CreateError::InvalidType {
                name: type_name.to_string(),
            })?;
        if ty.is_service {
            return Err(CreateError::InvalidType {
                name: type_name.to_string(),
            });
        }
        let entity_id = self.id_pool.get_id().ok_or(CreateError::NoId)?;
        let handle = self.next_load_id;
        self.next_load_id += 1;
        self.pending_loads.insert(
            handle,
            PendingLoad {
                entity_id,
                type_id: ty.id,
                type_name: type_name.to_string(),
            },
        );
        Ok(handle)
    }

    /// `LoadEntityHandler::handleMessage`: the DBApp answered a pending load.
    ///
    /// `Found` builds the base from the persisted data (`EntityType::create` →
    /// `Base::init`); `AlreadyActive`, a failure or an exception fire the
    /// callback with the mailbox or `None` and return the id via `putUsedID`
    /// (a no-op while recycling is off).
    pub fn on_load_reply(
        &mut self,
        request: LoadRequestId,
        reply: LoadReply,
    ) -> Option<LoadCallback> {
        let pending = self.pending_loads.remove(&request)?;
        let type_name = pending.type_name.clone();
        match reply {
            LoadReply::Found { db_id, stream } => {
                let built = self.build_from_stream(
                    pending.type_id,
                    &type_name,
                    pending.entity_id,
                    db_id,
                    &stream,
                );
                match built {
                    Ok(base) => Some(LoadCallback {
                        arg: CallbackArg::Loaded(base),
                        db_id,
                        was_active: false,
                    }),
                    Err(()) => {
                        // The record was checked out to us but the base could
                        // not be built; write it back so it can be loaded
                        // again. The callback still fires, with the dbID.
                        self.uncheckouts.push(Uncheckout {
                            type_id: pending.type_id,
                            db_id,
                            entity_id: pending.entity_id,
                        });
                        Some(LoadCallback {
                            arg: CallbackArg::None,
                            db_id,
                            was_active: false,
                        })
                    }
                }
            }
            LoadReply::AlreadyActive { db_id, mailbox } => {
                self.id_pool.put_used_id(pending.entity_id);
                Some(LoadCallback {
                    arg: CallbackArg::MailBox(mailbox),
                    db_id,
                    was_active: true,
                })
            }
            LoadReply::Failed { .. } | LoadReply::Exception => {
                self.id_pool.put_used_id(pending.entity_id);
                Some(LoadCallback {
                    arg: CallbackArg::None,
                    db_id: 0,
                    was_active: false,
                })
            }
        }
    }

    /// The number of pending `createBaseFromDB` requests.
    pub fn pending_load_count(&self) -> usize {
        self.pending_loads.len()
    }

    /// The `unCheckoutEntity` writes issued by failed loads, in order.
    pub fn uncheckouts(&self) -> &[Uncheckout] {
        &self.uncheckouts
    }

    /// `EntityType::create`: build a base from the persisted stream.
    ///
    /// The base section is read strictly in definition order (persistent
    /// base-data properties), with non-persistent base properties defaulted;
    /// a truncated stream fails the build. The cell section is read leniently
    /// (the reference captures it in `PyCellData` and parses it later), so a
    /// bad cell section leaves the remaining cell properties at their initial
    /// values without failing the load.
    fn build_from_stream(
        &mut self,
        type_id: u32,
        type_name: &str,
        entity_id: EntityId,
        db_id: DatabaseId,
        stream: &[u8],
    ) -> Result<BaseEntity, ()> {
        let ty = self.types.get(type_name).ok_or(())?;
        let mut reader = PersistenceReader::new(stream);

        let mut base_props = HashMap::new();
        for prop in &ty.properties {
            if !prop.base_data {
                continue;
            }
            let value = if prop.persistent {
                reader.read_prop(&prop.initial).ok_or(())?
            } else {
                prop.initial.clone()
            };
            base_props.insert(prop.name.clone(), value);
        }

        let mut cell_props = HashMap::new();
        if ty.can_be_on_cell {
            for prop in &ty.properties {
                if !prop.cell_data {
                    continue;
                }
                let value = if prop.persistent {
                    reader
                        .read_prop(&prop.initial)
                        .unwrap_or_else(|| prop.initial.clone())
                } else {
                    prop.initial.clone()
                };
                cell_props.insert(prop.name.clone(), value);
            }
        }

        let mut entity = BaseEntity {
            id: entity_id,
            type_id,
            type_name: type_name.to_string(),
            db_id,
            is_proxy: false,
            destroyed: false,
            in_destroy: false,
            base_props,
            cell_props,
            is_create_cell_pending: false,
            is_get_cell_pending: false,
            cell_entity_addr: None,
            cell_space_id: 0,
            callbacks: Vec::new(),
        };
        entity.callbacks.push(BaseCallback::Init {
            entity_id,
            type_name: type_name.to_string(),
        });
        entity
            .callbacks
            .push(BaseCallback::Initialised { entity_id });
        self.bases.insert(entity_id, entity.clone());
        Ok(entity)
    }

    /// `Base::destroy`: run the destroy sequence and return the id once.
    ///
    /// The order matches `base.cpp`: an already-destroyed base (or a
    /// re-entrant destroy) is a no-op; the script `onDestroy` runs; a base
    /// that has written to the database writes the flags back; the global
    /// bases are told; `discard` removes the base and returns the id via
    /// `putUsedID` (recorded here so the single return is observable).
    ///
    /// A repeated destroy returns [`DestroyOutcome::AlreadyDestroyed`] without
    /// returning the id again — the acceptance: 重复销毁不造成重复回收.
    pub fn destroy_base(
        &mut self,
        entity_id: EntityId,
        delete_from_db: bool,
        write_to_db: bool,
        log_off_from_db: bool,
    ) -> DestroyOutcome {
        if self.destroyed_ids.contains(&entity_id) {
            return DestroyOutcome::AlreadyDestroyed { id: entity_id };
        }
        let (in_destroy, db_id) = match self.bases.get(&entity_id) {
            Some(base) => (base.in_destroy, base.db_id),
            None => return DestroyOutcome::NotFound { id: entity_id },
        };
        if in_destroy {
            return DestroyOutcome::AlreadyDestroyed { id: entity_id };
        }
        self.bases.get_mut(&entity_id).unwrap().in_destroy = true;

        // `Script::call(onDestroy)`.
        self.destroy_calls.push(entity_id);

        // `hasWrittenToDB()`: write the flags back to the database.
        if db_id != 0 {
            let mut flags = 0u16;
            if log_off_from_db {
                flags |= write_db::LOG_OFF;
            }
            if delete_from_db {
                flags |= write_db::DELETE_FROM_DB;
            } else if write_to_db {
                flags |= write_db::BASE_CELL_DATA;
            }
            self.write_db.push(WriteDbEvent { entity_id, flags });
        }

        // `pGlobalBases()->onBaseDestroyed`.
        self.base_destroyed_calls.push(entity_id);

        // `discard`: remove the base and return the id.
        let mut base = self.bases.remove(&entity_id).unwrap();
        base.destroyed = true;
        base.in_destroy = false;
        self.id_pool.put_used_id(entity_id);
        self.returned_ids.push(entity_id);
        self.destroyed_ids.insert(entity_id);

        DestroyOutcome::Destroyed { id: entity_id }
    }

    /// The ids returned by `putUsedID` during destroys, in order.
    pub fn returned_ids(&self) -> &[EntityId] {
        &self.returned_ids
    }

    /// The `writeToDB` events issued by destroys, in order.
    pub fn write_db_events(&self) -> &[WriteDbEvent] {
        &self.write_db
    }

    /// The `onDestroy` script calls, in order.
    pub fn destroy_calls(&self) -> &[EntityId] {
        &self.destroy_calls
    }

    /// The `onBaseDestroyed` global-base notifications, in order.
    pub fn base_destroyed_calls(&self) -> &[EntityId] {
        &self.base_destroyed_calls
    }

    /// Whether a base has been destroyed.
    pub fn is_destroyed(&self, entity_id: EntityId) -> bool {
        self.destroyed_ids.contains(&entity_id)
    }
}

/// A read cursor over the persisted entity stream. The value encoding is
/// typed by the property's initial value, matching the compact stand-in
/// described in the module docs.
struct PersistenceReader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> PersistenceReader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let end = self.pos.checked_add(n)?;
        let slice = self.data.get(self.pos..end)?;
        self.pos = end;
        Some(slice)
    }

    fn read_u8(&mut self) -> Option<u8> {
        let byte = self.data.get(self.pos).copied();
        if byte.is_some() {
            self.pos += 1;
        }
        byte
    }

    /// Read one property value of the type of `expected`.
    fn read_prop(&mut self, expected: &PropValue) -> Option<PropValue> {
        match expected {
            PropValue::Int(_) => {
                let bytes = self.take(8)?;
                Some(PropValue::Int(i64::from_le_bytes(bytes.try_into().ok()?)))
            }
            PropValue::Float(_) => {
                let bytes = self.take(8)?;
                Some(PropValue::Float(f64::from_le_bytes(bytes.try_into().ok()?)))
            }
            PropValue::Str(_) => {
                let length = read_packed_int(self)?;
                if length < 0 {
                    return None;
                }
                let bytes = self.take(length as usize)?;
                let value = String::from_utf8(bytes.to_vec()).ok()?;
                Some(PropValue::Str(value))
            }
            PropValue::Bool(_) => Some(PropValue::Bool(self.read_u8()? != 0)),
            PropValue::Vec3(_) => {
                let mut value = [0.0f32; 3];
                for component in &mut value {
                    let bytes = self.take(4)?;
                    *component = f32::from_le_bytes(bytes.try_into().ok()?);
                }
                Some(PropValue::Vec3(value))
            }
        }
    }
}

/// `readPackedInt` (`binary_stream.hpp`): one byte, or `0xFF` followed by
/// three little-endian bytes for lengths of 255 and above.
fn read_packed_int(reader: &mut PersistenceReader<'_>) -> Option<i32> {
    let first = reader.read_u8()?;
    if first == 0xff {
        let b0 = reader.read_u8()? as i32;
        let b1 = reader.read_u8()? as i32;
        let b2 = reader.read_u8()? as i32;
        Some(b0 | (b1 << 8) | (b2 << 16))
    } else {
        Some(first as i32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mailbox_call::{MailboxCallError, MailboxMethodCall};

    fn account_type() -> EntityType {
        EntityType {
            id: 1,
            name: "Account".to_string(),
            can_be_on_base: true,
            is_service: false,
            is_proxy: false,
            can_be_on_cell: true,
            properties: vec![
                PropDesc::new("id", true, false, false, true, PropValue::Int(0)),
                PropDesc::new(
                    "name",
                    true,
                    false,
                    false,
                    true,
                    PropValue::Str(String::new()),
                ),
                PropDesc::new("pos", false, true, false, true, PropValue::Vec3([0.0; 3])),
                PropDesc::new("health", true, true, false, true, PropValue::Int(100)),
            ],
            methods: vec![],
        }
    }

    #[test]
    fn create_base_locally_assigns_type_and_init_args() {
        let mut creator = EntityCreator::new({
            let mut pool = IdPool::new(5, 10, 50, 100);
            pool.refill(&[10, 11, 12]);
            pool
        });
        creator.add_type(account_type());

        let mut args = CreateArgs::new();
        args.insert("id".to_string(), PropValue::Int(1));
        args.insert("name".to_string(), PropValue::Str("alice".to_string()));
        args.insert("pos".to_string(), PropValue::Vec3([1.0, 2.0, 3.0]));
        args.insert("health".to_string(), PropValue::Int(200));

        let base = creator.create_base_locally("Account", args).unwrap();

        // The type and id match; the id came from the pool.
        assert_eq!(base.type_name, "Account");
        assert_eq!(base.type_id, 1);
        assert_eq!(base.id, 10);
        assert_eq!(base.db_id, 0);
        assert!(!base.is_proxy);

        // Base properties: the base-data props stay; the both-flag `health`
        // moved to the cell data.
        assert_eq!(base.base_props.get("id"), Some(&PropValue::Int(1)));
        assert_eq!(
            base.base_props.get("name"),
            Some(&PropValue::Str("alice".to_string()))
        );
        assert!(!base.base_props.contains_key("pos"));
        assert!(!base.base_props.contains_key("health"));

        // Cell data: the defaults, the cell `pos`, and the moved `health`.
        assert_eq!(
            base.cell_props.get("pos"),
            Some(&PropValue::Vec3([1.0, 2.0, 3.0]))
        );
        assert_eq!(base.cell_props.get("health"), Some(&PropValue::Int(200)));
        assert_eq!(base.cell_props.get("spaceID"), Some(&PropValue::Int(0)));

        // The creation callback: `__init__` then `onEntityInitialised`.
        assert_eq!(
            base.callbacks,
            vec![
                BaseCallback::Init {
                    entity_id: 10,
                    type_name: "Account".to_string(),
                },
                BaseCallback::Initialised { entity_id: 10 },
            ]
        );

        // The entity is registered.
        assert!(creator.bases().contains_key(&10));
    }

    /// A service entity type, started on a ServiceApp.
    fn service_type() -> EntityType {
        EntityType {
            id: 9,
            name: "MailboxService".to_string(),
            can_be_on_base: true,
            is_service: true,
            is_proxy: false,
            can_be_on_cell: false,
            properties: vec![],
            methods: vec![],
        }
    }

    /// `ServiceStarter::run` starts a service entity directly, bypassing the
    /// `is_service` guard a login `createBaseLocally` applies.
    #[test]
    fn create_service_starts_a_service_entity() {
        let mut creator = creator_with_one_id(10);
        creator.add_type(service_type());

        let base = creator.create_service("MailboxService").expect("create");
        assert_eq!(base.type_name, "MailboxService");
        assert_eq!(base.type_id, 9);
        assert_eq!(base.id, 10);
        assert!(creator.bases().contains_key(&10));

        // A login creation still refuses the service type.
        let mut creator = creator_with_one_id(11);
        creator.add_type(service_type());
        assert_eq!(
            creator.create_base_locally("MailboxService", CreateArgs::new()),
            Err(CreateError::InvalidType {
                name: "MailboxService".to_string(),
            })
        );
        // A non-service type cannot be started as a service.
        let mut creator = creator_with_one_id(12);
        creator.add_type(account_type());
        assert_eq!(
            creator.create_service("Account"),
            Err(CreateError::InvalidType {
                name: "Account".to_string(),
            })
        );
        // An unknown type is invalid either way.
        let mut creator = creator_with_one_id(13);
        assert_eq!(
            creator.create_service("Missing"),
            Err(CreateError::InvalidType {
                name: "Missing".to_string(),
            })
        );
    }

    /// The create-success flow: begin, the reply succeeds, then the cell
    /// contacts the base. The pendings and callbacks mirror the reference.
    #[test]
    fn a_cell_create_success_ends_with_on_get_cell() {
        let mut creator = creator_with_one_id(10);
        creator.add_type(account_type());
        let base = creator
            .create_base_locally("Account", CreateArgs::new())
            .unwrap();

        // The request is eligible: both pendings go up.
        creator.begin_cell_creation(base.id).unwrap();
        let entity = creator.bases().get(&base.id).unwrap();
        assert!(entity.is_create_cell_pending);
        assert!(entity.is_get_cell_pending);
        assert_eq!(entity.cell_entity_addr, None);

        // The cell replies with the base's id: the create succeeded. Only
        // the create pending clears; the contact is still awaited.
        let outcome = creator.cell_creation_result(base.id, base.id == 10);
        assert_eq!(outcome, CellCreationResult::Created);
        let entity = creator.bases().get(&base.id).unwrap();
        assert!(!entity.is_create_cell_pending);
        assert!(entity.is_get_cell_pending);

        // The cell entity contacts the base: the address and space land, the
        // cell data is consumed and `onGetCell` fires.
        let cell_addr = crate::cell_data::BaseAddr {
            ip: 0x0a000002,
            port: 30001,
        };
        assert!(creator.cell_contact(base.id, 7, cell_addr));
        let entity = creator.bases().get(&base.id).unwrap();
        assert!(!entity.is_get_cell_pending);
        assert_eq!(entity.cell_entity_addr, Some(cell_addr));
        assert_eq!(entity.cell_space_id, 7);
        assert!(entity.cell_props.is_empty());
        assert_eq!(
            entity.callbacks.last(),
            Some(&BaseCallback::OnGetCell {
                entity_id: base.id,
                space_id: 7,
                cell_addr,
            })
        );
    }

    /// The create-failure flow: the reply fails (a different entity id, or a
    /// nub exception) clears both pendings and fires `onCreateCellFailure`.
    #[test]
    fn a_cell_create_failure_fires_on_create_cell_failure() {
        let mut creator = creator_with_one_id(10);
        creator.add_type(account_type());
        let base = creator
            .create_base_locally("Account", CreateArgs::new())
            .unwrap();

        creator.begin_cell_creation(base.id).unwrap();
        // The handler compares the reply's entity id with the base's: a
        // mismatch (or an exception) is a failure.
        let outcome = creator.cell_creation_result(base.id, base.id == 99);
        assert_eq!(outcome, CellCreationResult::Failed);

        let entity = creator.bases().get(&base.id).unwrap();
        assert!(!entity.is_create_cell_pending);
        assert!(!entity.is_get_cell_pending);
        assert_eq!(entity.cell_entity_addr, None);
        assert_eq!(
            entity.callbacks.last(),
            Some(&BaseCallback::OnCreateCellFailure { entity_id: base.id })
        );
    }

    /// A reply for a base with no create pending is ignored — the rare race
    /// the reference guards against — and never records a callback.
    #[test]
    fn a_reply_without_a_pending_create_is_ignored() {
        let mut creator = creator_with_one_id(10);
        creator.add_type(account_type());
        let base = creator
            .create_base_locally("Account", CreateArgs::new())
            .unwrap();

        // No `begin_cell_creation` call: the reply has nothing to land on.
        assert_eq!(
            creator.cell_creation_result(base.id, false),
            CellCreationResult::Ignored
        );
        let entity = creator.bases().get(&base.id).unwrap();
        assert!(!entity.is_get_cell_pending);
        assert!(
            !entity
                .callbacks
                .iter()
                .any(|c| matches!(c, BaseCallback::OnCreateCellFailure { .. }))
        );
    }

    /// The eligibility checks: destroyed, duplicated create and contact
    /// pending, an existing cell and a type without a cell script.
    #[test]
    fn ineligible_bases_are_rejected_before_any_pending() {
        let mut creator = EntityCreator::new({
            let mut pool = IdPool::new(5, 10, 50, 100);
            pool.refill(&[10, 11, 12]);
            pool
        });
        creator.add_type(account_type());

        // An unknown base.
        assert_eq!(
            creator.begin_cell_creation(99),
            Err(CellCreateError::UnknownBase)
        );

        // A type without a cell script cannot create a cell entity.
        let mut no_cell = account_type();
        no_cell.id = 3;
        no_cell.name = "NoCell".to_string();
        no_cell.can_be_on_cell = false;
        creator.add_type(no_cell.clone());
        let plain = creator
            .create_base_locally("NoCell", CreateArgs::new())
            .unwrap();
        assert_eq!(
            creator.begin_cell_creation(plain.id),
            Err(CellCreateError::NoCellScript)
        );
        // Nothing pending was set by the failed request.
        assert!(
            !creator
                .bases()
                .get(&plain.id)
                .unwrap()
                .is_create_cell_pending
        );

        // A duplicated create (the pending already set) is rejected.
        let base = creator
            .create_base_locally("Account", CreateArgs::new())
            .unwrap();
        creator.begin_cell_creation(base.id).unwrap();
        assert_eq!(
            creator.begin_cell_creation(base.id),
            Err(CellCreateError::AlreadyCreating)
        );

        // The contact pending (the reply succeeded but no contact yet).
        creator.cell_creation_result(base.id, true);
        assert_eq!(
            creator.begin_cell_creation(base.id),
            Err(CellCreateError::WaitingForContact)
        );

        // After the contact, the base already has a cell entity.
        let cell_addr = crate::cell_data::BaseAddr {
            ip: 0x0a000002,
            port: 30001,
        };
        creator.cell_contact(base.id, 7, cell_addr);
        assert_eq!(
            creator.begin_cell_creation(base.id),
            Err(CellCreateError::AlreadyHasCell)
        );
    }

    /// A base that has a cell and then loses it clears the cell reference and
    /// records `onLoseCell` (a non-proxy base gets no proxy notification).
    #[test]
    fn losing_a_cell_clears_the_reference_and_fires_on_lose_cell() {
        let mut creator = creator_with_one_id(10);
        creator.add_type(account_type());
        let base = creator
            .create_base_locally("Account", CreateArgs::new())
            .unwrap();
        creator.begin_cell_creation(base.id).unwrap();
        creator.cell_creation_result(base.id, true);
        let cell_addr = crate::cell_data::BaseAddr {
            ip: 0x0a000002,
            port: 30001,
        };
        creator.cell_contact(base.id, 7, cell_addr);

        assert!(creator.cell_lost(base.id, 7));
        let entity = creator.bases().get(&base.id).unwrap();
        assert_eq!(entity.cell_entity_addr, None);
        assert_eq!(entity.cell_space_id, 7);
        assert!(!entity.is_get_cell_pending);
        // No proxy notification for a non-proxy base.
        assert!(
            !entity
                .callbacks
                .iter()
                .any(|c| matches!(c, BaseCallback::CellEntityDestroyed { .. }))
        );
        assert_eq!(
            entity.callbacks.last(),
            Some(&BaseCallback::OnLoseCell { entity_id: base.id })
        );
    }

    /// A proxy base losing its cell is told `cellEntityDestroyed` before
    /// `onLoseCell`, in the reference order.
    #[test]
    fn a_proxy_base_is_told_its_cell_was_destroyed_first() {
        let mut creator = creator_with_one_id(10);
        let mut proxy_type = account_type();
        proxy_type.is_proxy = true;
        creator.add_type(proxy_type);
        let base = creator
            .create_base_locally("Account", CreateArgs::new())
            .unwrap();
        assert!(base.is_proxy);
        creator.begin_cell_creation(base.id).unwrap();
        creator.cell_creation_result(base.id, true);
        creator.cell_contact(
            base.id,
            7,
            crate::cell_data::BaseAddr {
                ip: 0x0a000002,
                port: 30001,
            },
        );

        creator.cell_lost(base.id, 7);
        let entity = creator.bases().get(&base.id).unwrap();
        let tail: Vec<&BaseCallback> = entity
            .callbacks
            .iter()
            .filter(|c| {
                matches!(
                    c,
                    BaseCallback::CellEntityDestroyed { .. } | BaseCallback::OnLoseCell { .. }
                )
            })
            .collect();
        assert_eq!(
            tail,
            vec![
                &BaseCallback::CellEntityDestroyed { entity_id: base.id },
                &BaseCallback::OnLoseCell { entity_id: base.id }
            ]
        );
    }

    /// A base with no cell has no transition: the loss is applied but records
    /// no callback.
    #[test]
    fn a_base_without_a_cell_has_no_loss_transition() {
        let mut creator = creator_with_one_id(10);
        creator.add_type(account_type());
        let base = creator
            .create_base_locally("Account", CreateArgs::new())
            .unwrap();

        assert!(creator.cell_lost(base.id, 1));
        let entity = creator.bases().get(&base.id).unwrap();
        assert_eq!(entity.cell_entity_addr, None);
        assert!(
            !entity
                .callbacks
                .iter()
                .any(|c| matches!(c, BaseCallback::OnLoseCell { .. }))
        );
    }

    /// An unknown or destroyed base cannot lose a cell.
    #[test]
    fn a_destroyed_or_unknown_base_cannot_lose_a_cell() {
        let mut creator = creator_with_one_id(10);
        creator.add_type(account_type());
        let base = creator
            .create_base_locally("Account", CreateArgs::new())
            .unwrap();

        assert!(!creator.cell_lost(99, 1));
        creator.destroy_base(base.id, false, false, false);
        assert!(!creator.cell_lost(base.id, 1));
    }

    #[test]
    fn an_unprovided_property_gets_its_initial_value() {
        let mut creator = EntityCreator::new({
            let mut pool = IdPool::new(5, 10, 50, 100);
            pool.refill(&[20]);
            pool
        });
        creator.add_type(account_type());

        let base = creator
            .create_base_locally("Account", CreateArgs::new())
            .unwrap();

        // The name defaults to "" and health to 100 (a both-flag prop moves to
        // the cell data).
        assert_eq!(
            base.base_props.get("name"),
            Some(&PropValue::Str(String::new()))
        );
        assert_eq!(base.cell_props.get("health"), Some(&PropValue::Int(100)));
    }

    #[test]
    fn a_service_or_non_base_type_is_rejected() {
        let mut creator = EntityCreator::new(IdPool::new(5, 10, 50, 100));
        creator.add_type(EntityType {
            id: 2,
            name: "Service".to_string(),
            can_be_on_base: true,
            is_service: true,
            is_proxy: false,
            can_be_on_cell: true,
            properties: vec![],
            methods: vec![],
        });
        creator.add_type(EntityType {
            id: 3,
            name: "CellOnly".to_string(),
            can_be_on_base: false,
            is_service: false,
            is_proxy: false,
            can_be_on_cell: true,
            properties: vec![],
            methods: vec![],
        });
        assert_eq!(
            creator.create_base_locally("Service", CreateArgs::new()),
            Err(CreateError::InvalidType {
                name: "Service".to_string()
            })
        );
        assert_eq!(
            creator.create_base_locally("CellOnly", CreateArgs::new()),
            Err(CreateError::InvalidType {
                name: "CellOnly".to_string()
            })
        );
    }

    #[test]
    fn an_empty_id_pool_fails_to_create() {
        let mut creator = EntityCreator::new(IdPool::new(5, 10, 50, 100));
        creator.add_type(account_type());
        assert_eq!(
            creator.create_base_locally("Account", CreateArgs::new()),
            Err(CreateError::NoId)
        );
        assert!(creator.bases().is_empty());
    }

    #[test]
    fn the_id_pool_requests_more_and_refills() {
        let mut pool = IdPool::new(2, 3, 10, 20);
        pool.refill(&[1, 2, 3]);
        assert_eq!(pool.get_id(), Some(1));
        assert_eq!(pool.get_id(), Some(2));
        // Below the low watermark, the pool asks for more.
        assert!(pool.pending_request());
        assert_eq!(pool.get_id(), Some(3));
        pool.refill(&[4, 5]);
        assert!(!pool.pending_request());
        assert_eq!(pool.get_id(), Some(4));
        // `putUsedID` is a no-op while recycling is off.
        pool.put_used_id(1);
        assert_eq!(pool.len(), 1);
    }

    // ---- BW-07-01-02 按 DBID 装载 Base ----

    fn player_type() -> EntityType {
        EntityType {
            id: 10,
            name: "Player".to_string(),
            can_be_on_base: true,
            is_service: false,
            is_proxy: false,
            can_be_on_cell: true,
            properties: vec![
                PropDesc::new(
                    "name",
                    true,
                    false,
                    false,
                    true,
                    PropValue::Str(String::new()),
                ),
                // A non-persistent base prop: defaulted on a load.
                PropDesc::new("session", true, false, false, false, PropValue::Int(0)),
                PropDesc::new("pos", false, true, false, true, PropValue::Vec3([0.0; 3])),
                PropDesc::new("health", true, true, false, true, PropValue::Int(100)),
            ],
            methods: vec![],
        }
    }

    /// Encode a property value in the model's persistence encoding (short
    /// strings only — the tests never hit the three-byte packed length).
    fn write_prop(buf: &mut Vec<u8>, value: &PropValue) {
        match value {
            PropValue::Int(v) => buf.extend_from_slice(&v.to_le_bytes()),
            PropValue::Float(v) => buf.extend_from_slice(&v.to_le_bytes()),
            PropValue::Str(s) => {
                buf.push(s.len() as u8);
                buf.extend_from_slice(s.as_bytes());
            }
            PropValue::Bool(v) => buf.push(*v as u8),
            PropValue::Vec3(v) => {
                for component in v {
                    buf.extend_from_slice(&component.to_le_bytes());
                }
            }
        }
    }

    /// A persisted `Player` stream: the base section (persistent base-data
    /// props: `name`, `health`) then the cell section (`pos`, `health`).
    fn player_stream(name: &str, pos: [f32; 3], health: i64) -> Vec<u8> {
        let mut buf = Vec::new();
        write_prop(&mut buf, &PropValue::Str(name.to_string()));
        write_prop(&mut buf, &PropValue::Int(health));
        write_prop(&mut buf, &PropValue::Vec3(pos));
        write_prop(&mut buf, &PropValue::Int(health));
        buf
    }

    fn creator_with_one_id(id: EntityId) -> EntityCreator {
        let mut creator = EntityCreator::new({
            let mut pool = IdPool::new(5, 10, 50, 100);
            pool.refill(&[id]);
            pool
        });
        creator.add_type(player_type());
        creator
    }

    #[test]
    fn an_existing_record_builds_the_base_and_fires_the_callback() {
        let mut creator = creator_with_one_id(50);
        let request = creator.create_base_from_db("Player", 42).unwrap();
        assert_eq!(creator.pending_load_count(), 1);

        let stream = player_stream("alice", [1.0, 2.0, 3.0], 120);
        let callback = creator
            .on_load_reply(request, LoadReply::Found { db_id: 42, stream })
            .unwrap();
        assert_eq!(creator.pending_load_count(), 0);

        let base = callback.entity().unwrap();
        assert_eq!(base.id, 50);
        assert_eq!(base.type_id, 10);
        assert_eq!(base.type_name, "Player");
        assert_eq!(base.db_id, 42);
        assert!(!base.is_proxy);

        // The persistent base props come from the stream; the non-persistent
        // `session` is defaulted (`insertDefaultProperties`).
        assert_eq!(
            base.base_props.get("name"),
            Some(&PropValue::Str("alice".to_string()))
        );
        assert_eq!(base.base_props.get("session"), Some(&PropValue::Int(0)));

        // The cell props carry the cell section.
        assert_eq!(
            base.cell_props.get("pos"),
            Some(&PropValue::Vec3([1.0, 2.0, 3.0]))
        );
        assert_eq!(base.cell_props.get("health"), Some(&PropValue::Int(120)));

        // `Base::init`: `__init__` then `onEntityInitialised`.
        assert_eq!(
            base.callbacks,
            vec![
                BaseCallback::Init {
                    entity_id: 50,
                    type_name: "Player".to_string(),
                },
                BaseCallback::Initialised { entity_id: 50 },
            ]
        );

        assert_eq!(callback.db_id, 42);
        assert!(!callback.was_active);
        assert!(creator.bases().contains_key(&50));
        assert!(creator.uncheckouts().is_empty());
    }

    #[test]
    fn a_missing_record_fires_the_callback_with_none() {
        let mut creator = creator_with_one_id(51);
        let request = creator.create_base_from_db("Player", 99).unwrap();

        let callback = creator
            .on_load_reply(
                request,
                LoadReply::Failed {
                    status: load_status::LOGIN_REJECTED_NO_SUCH_USER,
                    description: "No such user.".to_string(),
                },
            )
            .unwrap();

        assert!(callback.is_none());
        assert_eq!(callback.db_id, 0);
        assert!(!callback.was_active);
        assert!(creator.bases().is_empty());
        assert!(creator.uncheckouts().is_empty());
        assert_eq!(creator.pending_load_count(), 0);
    }

    #[test]
    fn an_already_active_record_fires_the_callback_with_the_mailbox() {
        let mut creator = creator_with_one_id(52);
        let request = creator.create_base_from_db("Player", 7).unwrap();

        let mailbox = EntityMailBoxRef::init(999, (0x7f00_0001, 20016), MailBoxComponent::Base, 10);
        let callback = creator
            .on_load_reply(request, LoadReply::AlreadyActive { db_id: 7, mailbox })
            .unwrap();

        assert_eq!(callback.mailbox(), Some(&mailbox));
        assert_eq!(callback.db_id, 7);
        assert!(callback.was_active);
        assert!(creator.bases().is_empty());
        assert_eq!(creator.pending_load_count(), 0);
    }

    #[test]
    fn a_truncated_stream_writes_the_record_back() {
        let mut creator = creator_with_one_id(53);
        let request = creator.create_base_from_db("Player", 42).unwrap();

        let mut stream = player_stream("alice", [1.0, 2.0, 3.0], 120);
        // 6 bytes for the name, then 4 of the 8 bytes of the base `health`:
        // the base section is truncated mid-value, so the build fails.
        stream.truncate(6 + 4);
        let callback = creator
            .on_load_reply(request, LoadReply::Found { db_id: 42, stream })
            .unwrap();

        // The build failed; the record is written back so it can be loaded
        // again, and the callback still fires once with the dbID.
        assert!(callback.is_none());
        assert_eq!(callback.db_id, 42);
        assert!(!callback.was_active);
        assert!(creator.bases().is_empty());
        assert_eq!(
            creator.uncheckouts(),
            &[Uncheckout {
                type_id: 10,
                db_id: 42,
                entity_id: 53,
            }]
        );
    }

    #[test]
    fn a_zero_db_id_is_rejected_before_allocating() {
        let mut creator = creator_with_one_id(60);
        assert_eq!(
            creator.create_base_from_db("Player", 0),
            Err(CreateError::InvalidDbId)
        );
        assert_eq!(creator.pending_load_count(), 0);
        assert_eq!(creator.id_pool().len(), 1); // no id was consumed
    }

    #[test]
    fn an_unknown_type_or_service_is_rejected() {
        let mut creator = EntityCreator::new(IdPool::new(5, 10, 50, 100));
        creator.add_type(EntityType {
            id: 3,
            name: "Svc".to_string(),
            can_be_on_base: true,
            is_service: true,
            is_proxy: false,
            can_be_on_cell: true,
            properties: vec![],
            methods: vec![],
        });
        assert_eq!(
            creator.create_base_from_db("Nope", 5),
            Err(CreateError::InvalidType {
                name: "Nope".to_string()
            })
        );
        assert_eq!(
            creator.create_base_from_db("Svc", 5),
            Err(CreateError::InvalidType {
                name: "Svc".to_string()
            })
        );
        assert_eq!(creator.pending_load_count(), 0);
    }

    #[test]
    fn an_empty_pool_fails_the_request() {
        let mut creator = EntityCreator::new(IdPool::new(5, 10, 50, 100));
        creator.add_type(player_type());
        assert_eq!(
            creator.create_base_from_db("Player", 5),
            Err(CreateError::NoId)
        );
        assert_eq!(creator.pending_load_count(), 0);
    }

    #[test]
    fn a_channel_exception_fires_the_callback_with_none() {
        let mut creator = creator_with_one_id(54);
        let request = creator.create_base_from_db("Player", 5).unwrap();
        let callback = creator
            .on_load_reply(request, LoadReply::Exception)
            .unwrap();
        assert!(callback.is_none());
        assert_eq!(callback.db_id, 0);
        assert_eq!(creator.pending_load_count(), 0);
    }

    #[test]
    fn an_unknown_request_is_ignored() {
        let mut creator = EntityCreator::new(IdPool::new(5, 10, 50, 100));
        creator.add_type(player_type());
        assert_eq!(creator.on_load_reply(0, LoadReply::Exception), None);
    }

    // ---- BW-07-01-04 销毁驻留实体并归还 ID ----

    #[test]
    fn destroying_a_base_returns_its_id_once_and_repeated_destroy_is_a_noop() {
        let mut creator = creator_with_one_id(60);
        let base = creator
            .create_base_locally("Player", CreateArgs::new())
            .unwrap();
        assert_eq!(base.id, 60);
        assert!(!base.destroyed);
        assert!(!base.in_destroy);
        assert!(creator.bases().contains_key(&60));

        let outcome = creator.destroy_base(60, false, true, true);
        assert_eq!(outcome, DestroyOutcome::Destroyed { id: 60 });
        assert_eq!(creator.returned_ids(), &[60]);
        assert!(creator.is_destroyed(60));
        assert!(!creator.bases().contains_key(&60));

        // A repeated destroy is a no-op: the id is not returned again and no
        // second callback runs.
        let outcome = creator.destroy_base(60, false, true, true);
        assert_eq!(outcome, DestroyOutcome::AlreadyDestroyed { id: 60 });
        assert_eq!(creator.returned_ids(), &[60]);
        assert_eq!(creator.destroy_calls().len(), 1);
        assert_eq!(creator.base_destroyed_calls().len(), 1);
    }

    #[test]
    fn destroying_a_persisted_base_writes_the_log_off_and_data_flags() {
        let mut creator = creator_with_one_id(70);
        let request = creator.create_base_from_db("Player", 42).unwrap();
        let stream = player_stream("alice", [1.0, 2.0, 3.0], 120);
        let callback = creator
            .on_load_reply(request, LoadReply::Found { db_id: 42, stream })
            .unwrap();
        assert_eq!(callback.entity().unwrap().id, 70);

        // `py_destroy`: writeToDB = hasWrittenToDB() = true, logOffFromDB =
        // true, deleteFromDB = false.
        let outcome = creator.destroy_base(70, false, true, true);
        assert_eq!(outcome, DestroyOutcome::Destroyed { id: 70 });
        assert_eq!(
            creator.write_db_events(),
            &[WriteDbEvent {
                entity_id: 70,
                flags: write_db::LOG_OFF | write_db::BASE_CELL_DATA,
            }]
        );
    }

    #[test]
    fn destroying_with_delete_from_db_writes_the_delete_flag() {
        let mut creator = creator_with_one_id(71);
        let request = creator.create_base_from_db("Player", 42).unwrap();
        let stream = player_stream("bob", [0.0; 3], 50);
        creator
            .on_load_reply(request, LoadReply::Found { db_id: 42, stream })
            .unwrap();

        let outcome = creator.destroy_base(71, true, true, true);
        assert_eq!(outcome, DestroyOutcome::Destroyed { id: 71 });
        assert_eq!(
            creator.write_db_events(),
            &[WriteDbEvent {
                entity_id: 71,
                flags: write_db::LOG_OFF | write_db::DELETE_FROM_DB,
            }]
        );
    }

    #[test]
    fn destroying_a_local_base_writes_nothing_to_the_database() {
        let mut creator = creator_with_one_id(72);
        creator
            .create_base_locally("Player", CreateArgs::new())
            .unwrap();
        // The local base has no database id, so `hasWrittenToDB()` is false
        // and nothing is written even when `write_to_db` is true.
        assert_eq!(
            creator.destroy_base(72, false, true, true),
            DestroyOutcome::Destroyed { id: 72 }
        );
        assert!(creator.write_db_events().is_empty());
    }

    #[test]
    fn destroying_an_unknown_id_is_not_found() {
        let mut creator = creator_with_one_id(73);
        assert_eq!(
            creator.destroy_base(999, false, true, true),
            DestroyOutcome::NotFound { id: 999 }
        );
        assert!(creator.returned_ids().is_empty());
        assert!(creator.destroy_calls().is_empty());
        assert!(creator.write_db_events().is_empty());
    }

    #[test]
    fn destroy_records_the_callbacks_in_order() {
        let mut creator = EntityCreator::new({
            let mut pool = IdPool::new(5, 10, 50, 100);
            pool.refill(&[80, 81]);
            pool
        });
        creator.add_type(player_type());
        creator
            .create_base_locally("Player", CreateArgs::new())
            .unwrap();
        creator
            .create_base_locally("Player", CreateArgs::new())
            .unwrap();

        assert_eq!(
            creator.destroy_base(80, false, true, true),
            DestroyOutcome::Destroyed { id: 80 }
        );
        assert_eq!(
            creator.destroy_base(81, false, true, true),
            DestroyOutcome::Destroyed { id: 81 }
        );

        assert_eq!(creator.destroy_calls(), &[80, 81]);
        assert_eq!(creator.base_destroyed_calls(), &[80, 81]);
        assert_eq!(creator.returned_ids(), &[80, 81]);
        assert!(creator.bases().is_empty());
    }

    // ---- BW-07-01-05 处理创建及装载失败 ----

    #[test]
    fn a_creation_failure_leaves_no_state_and_is_retryable() {
        let mut creator = creator_with_one_id(90);
        // Invalid type: the synchronous path has no callback, no base is made
        // and no id is consumed.
        assert_eq!(
            creator.create_base_locally("Nope", CreateArgs::new()),
            Err(CreateError::InvalidType {
                name: "Nope".to_string()
            })
        );
        assert!(creator.bases().is_empty());
        assert_eq!(creator.id_pool().len(), 1); // id 90 is still available.

        // A valid create after the failure succeeds — the state is retryable.
        let base = creator
            .create_base_locally("Player", CreateArgs::new())
            .unwrap();
        assert_eq!(base.id, 90);
        assert_eq!(creator.bases().len(), 1);
    }

    #[test]
    fn a_load_validation_failure_consumes_no_id_and_is_retryable() {
        let mut creator = creator_with_one_id(91);
        // `createBaseFromDB` validates the database id and the type before it
        // allocates the id.
        assert_eq!(
            creator.create_base_from_db("Player", 0),
            Err(CreateError::InvalidDbId)
        );
        assert_eq!(
            creator.create_base_from_db("Nope", 5),
            Err(CreateError::InvalidType {
                name: "Nope".to_string()
            })
        );
        assert_eq!(creator.id_pool().len(), 1); // no id consumed.
        assert_eq!(creator.pending_load_count(), 0);

        // A valid load after the failures succeeds.
        let request = creator.create_base_from_db("Player", 5).unwrap();
        let stream = player_stream("carol", [0.0; 3], 5);
        let callback = creator
            .on_load_reply(request, LoadReply::Found { db_id: 5, stream })
            .unwrap();
        assert_eq!(callback.entity().unwrap().id, 91);
    }

    #[test]
    fn a_load_failure_fires_the_callback_once_and_is_retryable() {
        let mut creator = EntityCreator::new({
            let mut pool = IdPool::new(5, 10, 50, 100);
            pool.refill(&[100, 101]);
            pool
        });
        creator.add_type(player_type());

        // First load: the record is missing.
        let request = creator.create_base_from_db("Player", 5).unwrap();
        let callback = creator
            .on_load_reply(
                request,
                LoadReply::Failed {
                    status: load_status::LOGIN_REJECTED_NO_SUCH_USER,
                    description: "No such user.".to_string(),
                },
            )
            .unwrap();
        assert!(callback.is_none());
        assert_eq!(callback.db_id, 0);
        assert!(!callback.was_active);
        assert_eq!(creator.pending_load_count(), 0);

        // The callback fires exactly once: the request is consumed.
        assert_eq!(creator.on_load_reply(request, LoadReply::Exception), None);

        // The state is retryable: a fresh load with the same db id issues a new
        // request and succeeds with the next id (recycling is off, so the
        // failed load's id is not reused).
        let retry = creator.create_base_from_db("Player", 5).unwrap();
        assert_ne!(retry, request);
        let stream = player_stream("dave", [1.0; 3], 10);
        let callback = creator
            .on_load_reply(retry, LoadReply::Found { db_id: 5, stream })
            .unwrap();
        assert_eq!(callback.entity().unwrap().id, 101);
    }

    #[test]
    fn a_channel_exception_fires_the_callback_once_and_is_retryable() {
        let mut creator = EntityCreator::new({
            let mut pool = IdPool::new(5, 10, 50, 100);
            pool.refill(&[92, 93]);
            pool
        });
        creator.add_type(player_type());

        let request = creator.create_base_from_db("Player", 5).unwrap();
        let callback = creator
            .on_load_reply(request, LoadReply::Exception)
            .unwrap();
        assert!(callback.is_none());
        assert_eq!(callback.db_id, 0);
        assert_eq!(creator.pending_load_count(), 0);
        assert_eq!(creator.on_load_reply(request, LoadReply::Exception), None);

        // A retry issues a fresh request with a new id (the failed load's id
        // was consumed and is not reused).
        let retry = creator.create_base_from_db("Player", 6).unwrap();
        assert_ne!(retry, request);
        let stream = player_stream("erin", [0.0; 3], 5);
        let callback = creator
            .on_load_reply(retry, LoadReply::Found { db_id: 6, stream })
            .unwrap();
        assert_eq!(callback.entity().unwrap().id, 93);
    }

    #[test]
    fn a_base_that_fails_to_build_fires_the_callback_once_with_the_db_id() {
        let mut creator = creator_with_one_id(93);
        let request = creator.create_base_from_db("Player", 42).unwrap();
        // Truncate the base section mid-value (6 bytes for the name, then 4 of
        // the 8 bytes of the base `health`): the strict base parse fails.
        let mut stream = player_stream("alice", [1.0, 2.0, 3.0], 120);
        stream.truncate(6 + 4);
        let callback = creator
            .on_load_reply(request, LoadReply::Found { db_id: 42, stream })
            .unwrap();
        assert!(callback.is_none());
        assert_eq!(callback.db_id, 42); // the build failure keeps the db id.
        assert!(!callback.was_active);
        // The record is written back so it can be loaded again.
        assert_eq!(creator.uncheckouts().len(), 1);
        assert_eq!(creator.uncheckouts()[0].db_id, 42);
        // The callback fired exactly once.
        assert_eq!(creator.on_load_reply(request, LoadReply::Exception), None);
    }

    /// A type with a base method `rename(string, int) -> int` for the
    /// mailbox delivery tests.
    fn rename_type() -> EntityType {
        let mut ty = account_type();
        ty.id = 5;
        ty.name = "Player".to_string();
        ty.methods = vec![
            MethodDesc::new(
                "rename",
                7,
                MethodComponent::Base,
                false,
                vec![PropValue::Str(String::new()), PropValue::Int(0)],
                vec![PropValue::Int(0)],
            ),
            MethodDesc::new(
                "moveTo",
                9,
                MethodComponent::Cell,
                false,
                vec![PropValue::Vec3([0.0; 3]), PropValue::Float(0.0)],
                vec![],
            ),
        ];
        ty
    }

    fn creator_with_rename() -> (EntityCreator, EntityId) {
        let mut pool = IdPool::new(5, 10, 50, 100);
        pool.refill(&[20]);
        let mut creator = EntityCreator::new(pool);
        creator.add_type(rename_type());
        let base = creator
            .create_base_locally("Player", Default::default())
            .expect("create");
        (creator, base.id)
    }

    fn creator_with_rename_and_cell() -> (EntityCreator, EntityId) {
        let (mut creator, id) = creator_with_rename();
        assert!(creator.cell_contact(
            id,
            1,
            BaseAddr {
                ip: 0x0a000001,
                port: 40001
            }
        ));
        (creator, id)
    }

    fn base_mailbox(id: EntityId) -> EntityMailBoxRef {
        EntityMailBoxRef::init(id, (0x7f000001, 20000), MailBoxComponent::Base, 5)
    }

    fn cell_via_base_mailbox(id: EntityId) -> EntityMailBoxRef {
        EntityMailBoxRef::init(id, (0x7f000001, 20000), MailBoxComponent::CellViaBase, 5)
    }

    fn move_to_call(id: EntityId) -> MailboxMethodCall {
        MailboxMethodCall {
            target_id: id,
            method_index: 9,
            source_entity_id: None,
            args: vec![PropValue::Vec3([1.0, 2.0, 3.0]), PropValue::Float(0.5)],
        }
    }

    #[test]
    fn a_mailbox_call_is_delivered_to_its_target_base() {
        let (mut creator, id) = creator_with_rename();
        let method = creator
            .deliver_mailbox_call(
                &base_mailbox(id),
                MailboxMethodCall {
                    target_id: id,
                    method_index: 7,
                    source_entity_id: None,
                    args: vec![PropValue::Str("bob".to_string()), PropValue::Int(3)],
                },
            )
            .expect("deliver");
        assert_eq!(method.name, "rename");
        assert_eq!(method.internal_index, 7);
        assert_eq!(method.return_kinds, vec![PropValue::Int(0)]);
        let callbacks = &creator.bases()[&id].callbacks;
        assert_eq!(
            callbacks.last(),
            Some(&BaseCallback::MethodCalled {
                entity_id: id,
                method_name: "rename".to_string(),
                method_index: 7,
                source_entity_id: None,
                args: vec![PropValue::Str("bob".to_string()), PropValue::Int(3)],
            })
        );
    }

    #[test]
    fn an_unknown_target_base_is_rejected() {
        let (mut creator, _id) = creator_with_rename();
        assert_eq!(
            creator.deliver_mailbox_call(
                &base_mailbox(99),
                MailboxMethodCall {
                    target_id: 99,
                    method_index: 7,
                    source_entity_id: None,
                    args: vec![PropValue::Str("bob".to_string()), PropValue::Int(3)],
                },
            ),
            Err(MailboxCallError::UnknownBase)
        );
    }

    #[test]
    fn an_unknown_method_index_is_rejected() {
        let (mut creator, id) = creator_with_rename();
        assert_eq!(
            creator.deliver_mailbox_call(
                &base_mailbox(id),
                MailboxMethodCall {
                    target_id: id,
                    method_index: 99,
                    source_entity_id: None,
                    args: vec![],
                },
            ),
            Err(MailboxCallError::UnknownMethod)
        );
    }

    #[test]
    fn a_cell_via_base_call_routes_to_the_tracked_cell() {
        let (mut creator, id) = creator_with_rename_and_cell();
        let method = creator
            .deliver_mailbox_call(&cell_via_base_mailbox(id), move_to_call(id))
            .expect("deliver");
        assert_eq!(method.name, "moveTo");
        let callbacks = &creator.bases()[&id].callbacks;
        assert_eq!(
            callbacks.last(),
            Some(&BaseCallback::CellMethodCalled {
                entity_id: id,
                cell_addr: BaseAddr {
                    ip: 0x0a000001,
                    port: 40001
                },
                method_name: "moveTo".to_string(),
                method_index: 9,
                source_entity_id: None,
                args: vec![PropValue::Vec3([1.0, 2.0, 3.0]), PropValue::Float(0.5)],
            })
        );
    }

    #[test]
    fn a_cell_call_after_migration_reaches_the_new_real() {
        let (mut creator, id) = creator_with_rename_and_cell();
        // The cell entity offloads to a new cellapp: the tracked address
        // switches, so a later call arrives at the new Real's role.
        creator
            .update_cell_address(
                id,
                2,
                BaseAddr {
                    ip: 0x0a000002,
                    port: 40002,
                },
            )
            .expect("migrate");
        let mailbox = creator.cell_mailbox(id).expect("cell mailbox");
        assert_eq!(mailbox.component, MailBoxComponent::Cell);
        assert_eq!(mailbox.addr, (0x0a000002, 40002));
        creator
            .deliver_mailbox_call(&cell_via_base_mailbox(id), move_to_call(id))
            .expect("deliver");
        let callbacks = &creator.bases()[&id].callbacks;
        assert_eq!(
            callbacks.last(),
            Some(&BaseCallback::CellMethodCalled {
                entity_id: id,
                cell_addr: BaseAddr {
                    ip: 0x0a000002,
                    port: 40002
                },
                method_name: "moveTo".to_string(),
                method_index: 9,
                source_entity_id: None,
                args: vec![PropValue::Vec3([1.0, 2.0, 3.0]), PropValue::Float(0.5)],
            })
        );
    }

    #[test]
    fn a_cell_call_without_a_cell_is_rejected() {
        let (mut creator, id) = creator_with_rename();
        assert_eq!(
            creator.deliver_mailbox_call(&cell_via_base_mailbox(id), move_to_call(id)),
            Err(MailboxCallError::NoCell)
        );
    }

    #[test]
    fn a_client_addressed_mailbox_is_out_of_the_model() {
        let (mut creator, id) = creator_with_rename();
        let mailbox = EntityMailBoxRef::init(id, (0x7f000001, 20000), MailBoxComponent::Client, 5);
        assert_eq!(
            creator.deliver_mailbox_call(&mailbox, move_to_call(id)),
            Err(MailboxCallError::ClientTarget)
        );
    }

    #[test]
    fn a_migrated_cell_updates_the_mailbox_address_without_callbacks() {
        let (mut creator, id) = creator_with_rename_and_cell();
        creator
            .update_cell_address(
                id,
                2,
                BaseAddr {
                    ip: 0x0a000002,
                    port: 40002,
                },
            )
            .expect("migrate");
        let base = &creator.bases()[&id];
        assert_eq!(
            base.cell_entity_addr,
            Some(BaseAddr {
                ip: 0x0a000002,
                port: 40002
            })
        );
        assert_eq!(base.cell_space_id, 2);
        // A pure move: the cell presence never changed, so no lose/get or
        // destroy notifications.
        assert_eq!(
            base.callbacks,
            vec![
                BaseCallback::Init {
                    entity_id: id,
                    type_name: "Player".to_string()
                },
                BaseCallback::Initialised { entity_id: id },
                BaseCallback::OnGetCell {
                    entity_id: id,
                    space_id: 1,
                    cell_addr: BaseAddr {
                        ip: 0x0a000001,
                        port: 40001
                    }
                },
            ]
        );
    }

    #[test]
    fn a_migration_without_a_cell_is_rejected() {
        let (mut creator, id) = creator_with_rename();
        assert_eq!(
            creator.update_cell_address(
                id,
                2,
                BaseAddr {
                    ip: 0x0a000002,
                    port: 40002
                }
            ),
            Err(CellMigrateError::NoCell)
        );
    }

    #[test]
    fn a_migration_with_a_none_address_is_a_loss() {
        let (mut creator, id) = creator_with_rename_and_cell();
        assert_eq!(
            creator.update_cell_address(id, 2, BaseAddr { ip: 0, port: 0 }),
            Err(CellMigrateError::LostCell)
        );
    }

    #[test]
    fn a_destroyed_base_cannot_migrate() {
        let (mut creator, id) = creator_with_rename_and_cell();
        creator.destroy_base(id, false, false, false);
        // The base is removed on destroy, so the migration is unknown.
        assert_eq!(
            creator.update_cell_address(
                id,
                2,
                BaseAddr {
                    ip: 0x0a000002,
                    port: 40002
                }
            ),
            Err(CellMigrateError::UnknownBase)
        );
    }

    #[test]
    fn an_unknown_base_cannot_migrate() {
        let (mut creator, _id) = creator_with_rename();
        assert_eq!(
            creator.update_cell_address(
                99,
                2,
                BaseAddr {
                    ip: 0x0a000002,
                    port: 40002
                }
            ),
            Err(CellMigrateError::UnknownBase)
        );
    }

    #[test]
    fn a_wrong_argument_count_is_rejected() {
        let (mut creator, id) = creator_with_rename();
        assert_eq!(
            creator.deliver_mailbox_call(
                &base_mailbox(id),
                MailboxMethodCall {
                    target_id: id,
                    method_index: 7,
                    source_entity_id: None,
                    args: vec![PropValue::Str("bob".to_string())],
                },
            ),
            Err(MailboxCallError::BadArgs {
                expected: 2,
                got: 1
            })
        );
    }

    #[test]
    fn a_wrong_argument_type_is_rejected() {
        let (mut creator, id) = creator_with_rename();
        assert_eq!(
            creator.deliver_mailbox_call(
                &base_mailbox(id),
                MailboxMethodCall {
                    target_id: id,
                    method_index: 7,
                    source_entity_id: None,
                    args: vec![
                        PropValue::Str("bob".to_string()),
                        PropValue::Str("bad".to_string())
                    ],
                },
            ),
            Err(MailboxCallError::BadArgKind { index: 1 })
        );
    }

    #[test]
    fn a_delivered_call_builds_a_matching_reply() {
        let (mut creator, id) = creator_with_rename();
        let method = creator
            .deliver_mailbox_call(
                &base_mailbox(id),
                MailboxMethodCall {
                    target_id: id,
                    method_index: 7,
                    source_entity_id: None,
                    args: vec![PropValue::Str("bob".to_string()), PropValue::Int(3)],
                },
            )
            .expect("deliver");
        // The reply a method would send: status 0 then its return value.
        let reply = crate::mailbox_call::MethodReply::success(&method, vec![PropValue::Int(9)])
            .expect("reply");
        let mut stream = foundation::stream::BinaryStream::new();
        reply.write_to(&mut stream, &method).expect("write");
        let parsed =
            crate::mailbox_call::MethodReply::read_from(&mut stream, &method).expect("read");
        assert_eq!(parsed.status, 0);
        assert_eq!(parsed.values, vec![PropValue::Int(9)]);
    }
}
