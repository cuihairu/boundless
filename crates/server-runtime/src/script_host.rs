//! BW-14-01-04: the script host ↔ service role wiring.
//!
//! The two stated prerequisites have landed: the cell runtime (BW-08) and
//! the interpreter faces (BW-03-03) — this module is the missing piece the
//! fixture scripts need: the base-app side loads `scripts/base/*.py`, the
//! engine constructs the script-class instance the way the reference does
//! (`PyType_GenericAlloc` + the type's `tp_init`, never `tp_new` — the
//! script bases refuse direct construction on purpose,
//! `server/baseapp/base.cpp:743-748`), and the `Base` method faces the
//! scripts call route into the port's request records.
//!
//! The bootstrap flow the acceptance pins, driven by the fixture's own
//! `scripts/base/Space.py` (loaded verbatim):
//!
//! * `Space.__init__` calls `createInNewSpace()` then
//!   `registerGlobally("DefaultSpace", self.onRegistered)` — the port
//!   records a `createEntityInNewSpace` request to the CellAppMgr
//!   (`base.cpp:3548`) and a `registerBaseGlobally` request
//!   (`base.cpp:4225`, `global_bases.cpp:284`), with the create/get
//!   pending flags set (`prepareForCellCreate`, `base.cpp:3269-3298`);
//! * the registration reply drives the callback the script handed over —
//!   once, with one bool (`MakeGlobalHandler`, `global_bases.cpp:228-252`),
//!   `True` only on a success code above zero, and the callback survives
//!   the base being destroyed in the meantime (`:240-242`);
//! * `onRegistered(False)` (the duplicate path) calls `destroyCellEntity()`
//!   — which is a **loud** `ValueError` while the cell create is still
//!   pending (`checkAssociatedCellEntity`, `base.cpp:3717-3760`), the
//!   behaviour the fixture's bootstrap really has;
//! * losing the cell runs `onLoseCell` (the `setCurrentCell`
//!   hadCell != haveCell dispatch, `base.cpp:1031-1044`; a missing script
//!   method is silent) and the script then calls `destroy()` — refused
//!   while a cell exists (`base.cpp:3986`), otherwise `onDestroy` fires,
//!   the global registration drops (`GlobalBases::onBaseDestroyed`,
//!   `global_bases.cpp:398`) and the base discards (`base.cpp:1553`).
//!
//! The cell side constructs the same way but dispatches `__init__` by name
//! after the properties would stream in (`entity.cpp:1916-1933`; the
//! nearby-entity arity sniff is part of the def wiring and stays out).
//!
//! Deltas kept visible:
//! - The `Base` script methods are stamped onto the existing `BigWorld.Base`
//!   type through `PyInstanceMethod_New` wrappers rather than a `tp_methods`
//!   table — the type object already exists (BW-16-03-02), and CPython has
//!   no post-creation method-table API. From a script the calls bind
//!   identically.
//! - The def-driven entity-type registry (`EntityType::init`,
//!   `cellapp/entity_type.cpp:264`, `entities.xml` parse, the `IGameDelegate`
//!   fallback classes) is the def wiring's face and stays out: the role's
//!   classes are imported by name and the subclass rule against the
//!   registered base is enforced the way `importEntityClass` enforces it
//!   (`baseapp/entity_type.cpp:124-131`).
//! - `canBeOnCell` is the `scripts/cell/<Name>.py` file probe
//!   (`entity_description.cpp:40-55`), `.py` only — the `.pyc/.pyo/.pyd`
//!   spellings have no port home yet.
//! - The DB write the reference's `Base::destroy` initiates runs behind the
//!   `hasWrittenToDB` gate (`base.cpp:1515-1538`); the base-write face
//!   itself is BW-14-01-05's `base_write_db`.
//! - The Mercury traffic is not modelled: the module records the requests
//!   into an outbox, matching the port's other server modules.

use crate::baseappmgr::GlobalBaseMailbox;
use crate::entity_creator::EntityId;
use crate::global_bases::GlobalBases;
use python_host::pyref::PyRef;
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::PathBuf;

// ---------------------------------------------------------------------------
// The pure engine half: one base entity's flags and transitions.
// ---------------------------------------------------------------------------

/// The engine-owned half of a base entity: the flags the script methods
/// read and write (`base.cpp`'s `isCreateCellPending_` family). The script
/// object reference lives in the host, not here, so the transitions stay
/// unit-testable without an interpreter.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BaseFlags {
    /// The space the cell entity lives in (`spaceID_`).
    pub space_id: u32,
    /// `pCellEntityMailBox_` carries a live address.
    pub has_cell: bool,
    /// A cell create is in flight (`isCreateCellPending_`).
    pub is_create_cell_pending: bool,
    /// The created cell has not contacted us yet (`isGetCellPending_`).
    pub is_get_cell_pending: bool,
    /// A cell destroy is in flight (`isDestroyCellPending_`).
    pub is_destroy_cell_pending: bool,
    /// `isDestroyed_`: the base has discarded.
    pub is_destroyed: bool,
    /// `inDestroy_`: the reentry guard of `Base::destroy`.
    pub in_destroy: bool,
    /// `hasWrittenToDB()`: the gate behind which `destroy` writes.
    pub has_written_to_db: bool,
}

/// The outcome of `Base::checkAssociatedCellEntity`
/// (`base.cpp:3717-3760`) — each failure is one exact `ValueError`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CellEntityCheck {
    /// The base is fine for the requested direction.
    Ok,
    /// `isDestroyed_`: "is destroyed".
    Destroyed,
    /// `isCreateCellPending_`: "is in the process of creating ...".
    CreatePending,
    /// `isGetCellPending_`: "is waiting for contact from ...".
    GetPending,
    /// `havingEntityGood` with a live cell: "already has ...".
    AlreadyHasCell,
    /// `havingEntityGood` without a live cell: "does not have ...".
    NoCell,
}

/// The text of `checkAssociatedCellEntity`'s `ValueError`s, verbatim —
/// `prefix` is the calling method's own (`"Base.createEntityInNewSpace: "`).
pub fn associated_cell_entity_error(check: CellEntityCheck, id: EntityId, prefix: &str) -> String {
    match check {
        CellEntityCheck::Ok => String::new(),
        CellEntityCheck::Destroyed => format!("{prefix}Base {id} is destroyed"),
        CellEntityCheck::CreatePending => {
            format!("{prefix}Base {id} is in the process of creating an associated cell entity.")
        }
        CellEntityCheck::GetPending => format!(
            "{prefix}Base {id} is waiting for contact from its created associated cell entity."
        ),
        CellEntityCheck::AlreadyHasCell => {
            format!("{prefix}Base {id} already has an associated cell entity.")
        }
        CellEntityCheck::NoCell => {
            format!("{prefix}Base {id} does not have an associated cell entity.")
        }
    }
}

/// `Base::checkAssociatedCellEntity` (`base.cpp:3717-3760`): the state
/// ladder in the reference's order — destroyed, then the two pending
/// creates, then the direction check.
pub fn check_associated_cell_entity(
    flags: &BaseFlags,
    having_entity_good: bool,
) -> CellEntityCheck {
    if flags.is_destroyed {
        CellEntityCheck::Destroyed
    } else if flags.is_create_cell_pending {
        CellEntityCheck::CreatePending
    } else if flags.is_get_cell_pending {
        CellEntityCheck::GetPending
    } else if !having_entity_good && flags.has_cell {
        CellEntityCheck::AlreadyHasCell
    } else if having_entity_good && !flags.has_cell {
        CellEntityCheck::NoCell
    } else {
        CellEntityCheck::Ok
    }
}

/// The failure of `Base::prepareForCellCreate` (`base.cpp:3269-3298`):
/// the associated-cell ladder, or the type has no cell script at all
/// (`canBeOnCell` false → the `AttributeError` arm).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CellCreateError {
    /// The associated-cell ladder failed; carries the rung.
    Check(CellEntityCheck),
    /// `"%sBase %d has no cell script."` — the `AttributeError` arm.
    NoCellScript,
}

/// `Base::prepareForCellCreate` (`base.cpp:3269-3298`): the ladder runs
/// with `havingEntityGood = false` (a second create is the error), then
/// the cell-script probe, then the two pending flags set together.
pub fn prepare_for_cell_create(
    flags: &mut BaseFlags,
    can_be_on_cell: bool,
) -> Result<(), CellCreateError> {
    let check = check_associated_cell_entity(flags, false);
    if check != CellEntityCheck::Ok {
        return Err(CellCreateError::Check(check));
    }
    if !can_be_on_cell {
        return Err(CellCreateError::NoCellScript);
    }
    flags.is_create_cell_pending = true;
    flags.is_get_cell_pending = true;
    Ok(())
}

/// The outcome of `Base::cellCreationResult` (`base.cpp:3757-3790`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CellCreationOutcome {
    /// The base was destroyed meanwhile: nothing runs.
    InertDestroyed,
    /// No create was pending (the rare lost race): the failure is only
    /// logged.
    InertNotPending,
    /// The pending flags cleared; the script is not called.
    Cleared,
    /// The create failed: the pending flags cleared and the engine calls
    /// the script's `onCreateCellFailure` (tolerating its absence).
    FailedCallsScript,
}

/// `Base::cellCreationResult` (`base.cpp:3757-3790`): a destroyed base is
/// inert, a lost race only logs, a success just clears the create pending
/// (the get pending waits for the contact), a failure also drops the get
/// pending and calls the script.
pub fn cell_creation_result(flags: &mut BaseFlags, success: bool) -> CellCreationOutcome {
    if flags.is_destroyed {
        return CellCreationOutcome::InertDestroyed;
    }
    if !flags.is_create_cell_pending {
        return CellCreationOutcome::InertNotPending;
    }
    flags.is_create_cell_pending = false;
    if success {
        CellCreationOutcome::Cleared
    } else {
        flags.is_get_cell_pending = false;
        CellCreationOutcome::FailedCallsScript
    }
}

/// `Base::setCurrentCell`'s transition (`base.cpp:1028-1044`): the flags
/// move first; the return says whether the script callback fires and which
/// one — `Some(true)` is `onGetCell`, `Some(false)` is `onLoseCell`, `None`
/// is the no-change path.
pub fn set_current_cell(flags: &mut BaseFlags, space_id: u32, have_cell: bool) -> Option<bool> {
    let had_cell = flags.has_cell;
    flags.space_id = space_id;
    flags.has_cell = have_cell;
    if had_cell == have_cell {
        return None;
    }
    flags.is_get_cell_pending = false;
    if have_cell {
        flags.is_create_cell_pending = false;
    }
    Some(have_cell)
}

/// The steps `Base::destroy` runs (`base.cpp:1488-1560`), in order, so the
/// host's drive replays the same sequence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DestroyStep {
    /// The script's `onDestroy` (tolerated missing).
    CallOnDestroy,
    /// The backup is told to stop.
    StopBackup,
    /// `writeToDB` with these flags — only behind `hasWrittenToDB`.
    WriteDb,
    /// The global registrations this base owned drop.
    DeregisterGlobals,
    /// `discard()`: the base is destroyed and its script object released.
    Discard,
}

/// The `Base::destroy` failure arms (`base.cpp:3975-3995`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DestroyError {
    /// `hasCellEntity || isGetCellPending`: "Still has cell entity. ..."
    StillHasCell,
    /// `isDestroyed_`: "Base entity already destroyed".
    AlreadyDestroyed,
}

/// `py_destroy`'s guards (`base.cpp:3984-3995`): a live (or pending) cell
/// refuses with the destroy-cell hint, a discarded base refuses outright.
pub fn check_destroy_allowed(flags: &BaseFlags) -> Result<(), DestroyError> {
    if flags.has_cell || flags.is_get_cell_pending {
        Err(DestroyError::StillHasCell)
    } else if flags.is_destroyed {
        Err(DestroyError::AlreadyDestroyed)
    } else {
        Ok(())
    }
}

/// `Base::destroy(deleteFromDB, writeToDB, logOffFromDB)`
/// (`base.cpp:1488-1560`): the step list, in the reference's order. A
/// destroyed base records nothing; the `inDestroy_` latch itself is the
/// drive's (`host_destroy_base` raises it before the script dispatch,
/// `base.cpp:1490`). The DB write only joins behind `hasWrittenToDB`.
pub fn destroy_steps(
    flags: &mut BaseFlags,
    delete_from_db: bool,
    write_to_db: bool,
    log_off_from_db: bool,
) -> Vec<DestroyStep> {
    if flags.is_destroyed {
        return Vec::new();
    }
    flags.in_destroy = true;
    let mut steps = vec![DestroyStep::CallOnDestroy, DestroyStep::StopBackup];
    if flags.has_written_to_db {
        steps.push(DestroyStep::WriteDb);
        let _ = (delete_from_db, write_to_db, log_off_from_db);
    }
    steps.push(DestroyStep::DeregisterGlobals);
    steps.push(DestroyStep::Discard);
    // discard(): the base is gone the moment the steps run.
    flags.in_destroy = false;
    flags.is_destroyed = true;
    steps
}

// ---------------------------------------------------------------------------
// The host: records, outbox, and the drives.
// ---------------------------------------------------------------------------

/// One recorded request the reference would put on the wire.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BaseOutbound {
    /// `CellAppMgrInterface::createEntityInNewSpace` (`base.cpp:3589`).
    CreateEntityInNewSpace {
        base: EntityId,
        should_prefer_this_machine: bool,
    },
    /// `CellAppInterface::destroyEntityArgs`, straight to the owning
    /// CellApp (`base.cpp:4057-4060`).
    DestroyCellEntity { base: EntityId },
    /// `BaseAppMgrInterface::registerBaseGlobally`
    /// (`baseappmgr_gateway.cpp:101`).
    RegisterBaseGlobally { base: EntityId, key: String },
    /// The deregister requests `GlobalBases::onBaseDestroyed` returns
    /// (`global_bases.cpp:398`).
    DeregisterBaseGlobally { key: String },
}

/// The script callback a `registerGlobally` handed over, held by the
/// request handler — not by the base — so it survives the base's destroy
/// (`global_bases.cpp:240-242`).
pub struct PendingRegistration {
    pub base: EntityId,
    pub key: String,
    pub callback: PyRef,
}

/// The script method the engine called and its base.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScriptSignal {
    /// `onDestroy` fired.
    OnDestroy(EntityId),
    /// `onGetCell` fired.
    OnGetCell(EntityId),
    /// `onLoseCell` fired.
    OnLoseCell(EntityId),
    /// `onCreateCellFailure` fired.
    OnCreateCellFailure(EntityId),
}

/// The host one script-running process carries: the base records, the
/// request outbox, the pending registrations and the global-base mirror.
#[derive(Default)]
pub struct ScriptHost {
    /// The flags per base, keyed by id.
    pub flags: BTreeMap<EntityId, BaseFlags>,
    /// The script objects the engine holds, keyed by id.
    scripts: BTreeMap<EntityId, PyRef>,
    /// The instance-pointer index the script method handlers resolve.
    ptr_index: BTreeMap<usize, EntityId>,
    /// The recorded wire requests, in call order.
    pub outbound: Vec<BaseOutbound>,
    /// The `registerGlobally` callbacks awaiting their reply.
    pub pending_registrations: Vec<PendingRegistration>,
    /// The BaseApp's global-base dictionary (`GlobalBases`).
    pub global_bases: GlobalBases,
    /// The entity type name per base, the probe key of `canBeOnCell`.
    pub type_names: BTreeMap<EntityId, String>,
    /// The Python errors the script methods raised, in order.
    pub script_errors: Vec<String>,
    /// The script methods the engine drove, in order.
    pub script_signals: Vec<ScriptSignal>,
    /// The ids handed out so far.
    next_id: EntityId,
    /// The res root the cell-script probe reads.
    scripts_root: Option<PathBuf>,
}

impl ScriptHost {
    /// A fresh host.
    pub fn new() -> Self {
        Self::default()
    }

    /// The res root the `canBeOnCell` probe reads.
    pub fn set_scripts_root(&mut self, root: PathBuf) {
        self.scripts_root = Some(root);
    }

    /// `canBeOnCell` (`entity_description.cpp:40-55`): the cell script
    /// file exists for the entity type's own name.
    pub fn can_be_on_cell(&self, name: &str) -> bool {
        self.scripts_root
            .as_ref()
            .map(|root| {
                root.join("scripts/cell")
                    .join(format!("{name}.py"))
                    .exists()
            })
            .unwrap_or(false)
    }

    /// Allocates the next base id.
    pub fn allocate_id(&mut self) -> EntityId {
        self.next_id += 1;
        self.next_id
    }

    /// Registers a freshly allocated script instance: the flags record and
    /// the engine's strong reference, indexed by the instance pointer.
    /// `type_name` is the entity type's own name — the `canBeOnCell`
    /// probe key.
    pub fn register_script(&mut self, id: EntityId, type_name: &str, instance: PyRef) {
        self.ptr_index.insert(instance.as_ptr() as usize, id);
        self.scripts.insert(id, instance);
        self.type_names.insert(id, type_name.to_string());
        self.flags.insert(id, BaseFlags::default());
    }

    /// The base id an instance pointer belongs to, for the method handlers.
    pub fn base_of_ptr(&self, ptr: usize) -> Option<EntityId> {
        self.ptr_index.get(&ptr).copied()
    }

    /// The engine half of `Base::py_createInNewSpace`
    /// (`base.cpp:3548-3600`): the prepare ladder, the probe, the request
    /// record. `Err` carries the exact error text.
    pub fn create_in_new_space(
        &mut self,
        id: EntityId,
        should_prefer_this_machine: bool,
    ) -> Result<(), String> {
        let prefix = "Base.createEntityInNewSpace: ";
        let type_name = self.type_names.get(&id).cloned().unwrap_or_default();
        let can_be_on_cell = self.can_be_on_cell(&type_name);
        let Some(flags) = self.flags.get_mut(&id) else {
            return Err(format!("{prefix}Base {id} is destroyed"));
        };
        match prepare_for_cell_create(flags, can_be_on_cell) {
            Ok(()) => {}
            Err(CellCreateError::Check(check)) => {
                return Err(associated_cell_entity_error(check, id, prefix));
            }
            Err(CellCreateError::NoCellScript) => {
                return Err(format!("{prefix}Base {id} has no cell script."));
            }
        }
        self.outbound.push(BaseOutbound::CreateEntityInNewSpace {
            base: id,
            should_prefer_this_machine,
        });
        Ok(())
    }

    /// The engine half of `Base::py_registerGlobally`
    /// (`base.cpp:4225-4273`): the key is carried as its string and the
    /// callback joins the pending list; the request record rides the
    /// outbox. `Err` carries the exact error text.
    pub fn register_globally(
        &mut self,
        id: EntityId,
        key: &str,
        callback: PyRef,
    ) -> Result<(), String> {
        self.pending_registrations.push(PendingRegistration {
            base: id,
            key: key.to_string(),
            callback,
        });
        self.outbound.push(BaseOutbound::RegisterBaseGlobally {
            base: id,
            key: key.to_string(),
        });
        Ok(())
    }

    /// The bookkeeping half of `MakeGlobalHandler::handleMessage`
    /// (`global_bases.cpp:228-252`): the success code answers the pending
    /// registration with the same key; on success the dictionary gains the
    /// entry. The callback hands back to the drive — it runs once, with the
    /// host borrow released (`host_on_register_reply`), even if the base
    /// was destroyed in the meantime (`global_bases.cpp:240-242`). `None`
    /// is no pending under the key.
    pub fn on_register_reply(&mut self, key: &str, success_code: i8) -> Option<(PyRef, bool)> {
        let index = self
            .pending_registrations
            .iter()
            .position(|pending| pending.key == key)?;
        let pending = self.pending_registrations.remove(index);
        let succeeded = success_code > 0;
        if succeeded {
            let mailbox = GlobalBaseMailbox {
                entity: pending.base,
                addr: (1, 10001),
            };
            self.global_bases
                .on_register_ack(pending.base, &pending.key, mailbox, true);
        }
        Some((pending.callback, succeeded))
    }

    /// `Base::cellCreationResult` (`base.cpp:3757-3790`) plus the failure
    /// arm's `onCreateCellFailure` script call.
    pub fn on_cell_creation_result(&mut self, id: EntityId, success: bool) {
        let outcome = match self.flags.get_mut(&id) {
            Some(flags) => cell_creation_result(flags, success),
            None => return,
        };
        if outcome == CellCreationOutcome::FailedCallsScript {
            self.script_signals
                .push(ScriptSignal::OnCreateCellFailure(id));
        }
    }

    /// `Base::setCurrentCell`'s bookkeeping (`base.cpp:954-1050`): the
    /// flags transition and the drive record; `Some(fire)` names the script
    /// callback the drive dispatches with the borrow released
    /// (`host_set_current_cell`) — a missing script method is silent
    /// (`PyErr_Clear`, `base.cpp:1038-1040`).
    pub fn set_current_cell(
        &mut self,
        id: EntityId,
        space_id: u32,
        have_cell: bool,
    ) -> Option<bool> {
        let flags = self.flags.get_mut(&id)?;
        let fire = set_current_cell(flags, space_id, have_cell)?;
        self.script_signals.push(if fire {
            ScriptSignal::OnGetCell(id)
        } else {
            ScriptSignal::OnLoseCell(id)
        });
        Some(fire)
    }

    /// The engine half of `Base::py_destroyCellEntity`
    /// (`base.cpp:4037-4070`): the get-pending bypass, the loud ladder,
    /// the idempotence guard, the request record. `Err` carries the exact
    /// error text.
    pub fn destroy_cell_entity(&mut self, id: EntityId) -> Result<(), String> {
        let prefix = "Base.destroyCellEntity: ";
        let Some(flags) = self.flags.get(&id) else {
            return Err(format!("{prefix}Base {id} is destroyed"));
        };
        if !flags.is_get_cell_pending {
            let check = check_associated_cell_entity(flags, true);
            if check != CellEntityCheck::Ok {
                return Err(associated_cell_entity_error(check, id, prefix));
            }
        }
        if self
            .flags
            .get(&id)
            .map(|flags| flags.is_destroy_cell_pending)
            .unwrap_or(false)
        {
            return Ok(());
        }
        if let Some(flags) = self.flags.get_mut(&id) {
            flags.is_destroy_cell_pending = true;
        }
        self.outbound
            .push(BaseOutbound::DestroyCellEntity { base: id });
        Ok(())
    }

    /// `Base::destroy` (`base.cpp:1488-1560`): the guards, then the step
    /// sequence — the script's `onDestroy`, the global deregisters, and
    /// the discard that releases the script object. `Err` carries the
    /// exact error text of the refused arm.
    pub fn destroy_base(
        &mut self,
        id: EntityId,
        delete_from_db: bool,
        write_to_db: bool,
        log_off_from_db: bool,
    ) -> Result<Vec<DestroyStep>, String> {
        let Some(flags) = self.flags.get(&id) else {
            return Err("Base entity already destroyed".to_string());
        };
        match check_destroy_allowed(flags) {
            Ok(()) => {}
            Err(DestroyError::StillHasCell) => {
                return Err("Still has cell entity. Use Base.destroyCellEntity".to_string());
            }
            Err(DestroyError::AlreadyDestroyed) => {
                return Err("Base entity already destroyed".to_string());
            }
        }
        let flags = self.flags.get_mut(&id).expect("checked above");
        let steps = destroy_steps(flags, delete_from_db, write_to_db, log_off_from_db);
        for key in self.global_bases.on_base_destroyed(id) {
            self.outbound
                .push(BaseOutbound::DeregisterBaseGlobally { key });
        }
        if steps.contains(&DestroyStep::Discard)
            && let Some(script) = self.scripts.remove(&id)
        {
            self.ptr_index.remove(&(script.as_ptr() as usize));
            unsafe { clear_script_dict(script.as_ptr()) };
            drop(script);
        }
        Ok(steps)
    }
}

// ---------------------------------------------------------------------------
// The interpreter-facing half. One interpreter per process, so the host
// rides a thread-local the script method handlers reach.
// ---------------------------------------------------------------------------

thread_local! {
    static HOST: RefCell<ScriptHost> = RefCell::new(ScriptHost::new());
}

/// Runs `body` with the process host.
pub fn with_host<R>(body: impl FnOnce(&mut ScriptHost) -> R) -> R {
    HOST.with(|host| body(&mut host.borrow_mut()))
}

/// The engine drive of `Base::setCurrentCell` (`base.cpp:954-1050`): the
/// flags move under the host borrow, then the script callback runs with
/// the borrow released — the fixture's `onLoseCell` calls `destroy()`, and
/// a dispatch under the `RefCell` would deadlock the re-entry.
pub fn host_set_current_cell(id: EntityId, space_id: u32, have_cell: bool) {
    let fire = with_host(|host| host.set_current_cell(id, space_id, have_cell));
    let Some(fire) = fire else {
        return;
    };
    let name = if fire { "onGetCell" } else { "onLoseCell" };
    let instance = with_host(|host| host.scripts.get(&id).cloned());
    if let Some(instance) = instance {
        // SAFETY: the interpreter is up on the driving thread.
        unsafe { call_script_method(instance.as_ptr(), name) };
    }
}

/// The engine drive of the `registerBaseGlobally` reply
/// (`global_bases.cpp:228-252`): the bookkeeping under the borrow, the
/// callback with it released — the duplicate path's `onRegistered(False)`
/// re-enters the script faces through `destroyCellEntity()`.
pub fn host_on_register_reply(key: &str, success_code: i8) -> bool {
    let Some((callback, succeeded)) = with_host(|host| host.on_register_reply(key, success_code))
    else {
        return false;
    };
    // SAFETY: the interpreter is up on the driving thread.
    unsafe { call_bool_callback(callback.as_ptr(), succeeded) };
    true
}

/// The engine drive of `Base.destroy` (`base.cpp:3975-3995` +
/// `base.cpp:1488-1560`): the guards and the `inDestroy_` latch under the
/// borrow, the script's `onDestroy` with the borrow released (it may
/// re-enter), then the steps and the discard.
pub fn host_destroy_base(
    id: EntityId,
    delete_from_db: bool,
    write_to_db: bool,
    log_off_from_db: bool,
) -> Result<Vec<DestroyStep>, String> {
    let refused = |error| match error {
        DestroyError::StillHasCell => {
            "Still has cell entity. Use Base.destroyCellEntity".to_string()
        }
        DestroyError::AlreadyDestroyed => "Base entity already destroyed".to_string(),
    };
    let dispatch = with_host(|host| {
        let Some(flags) = host.flags.get_mut(&id) else {
            return Err("Base entity already destroyed".to_string());
        };
        // A drive already running — a destroy from inside `onDestroy` — is
        // the `inDestroy_` no-op (`base.cpp:1490`).
        if flags.in_destroy {
            return Ok(None);
        }
        check_destroy_allowed(flags).map_err(refused)?;
        flags.in_destroy = true;
        Ok(host.scripts.get(&id).cloned())
    })?;
    let Some(instance) = dispatch else {
        return Ok(Vec::new());
    };
    // SAFETY: the interpreter is up on the driving thread; a missing
    // method is the silent arm.
    unsafe { call_script_method(instance.as_ptr(), "onDestroy") };
    with_host(|host| host.destroy_base(id, delete_from_db, write_to_db, log_off_from_db))
}

/// The `BigWorld.Base` type object, borrowed for the call's scope.
///
/// # Safety
///
/// The interpreter must be up and this thread must own the GIL.
unsafe fn base_type() -> *mut python_host::capi::PyObject {
    use python_host::capi;
    // SAFETY: the interpreter is up; the module object is borrowed — the
    // interpreter's own startup registered `BigWorld`, so the module is
    // always there and only the `Base` attribute can miss.
    unsafe {
        capi::PyObject_GetAttrString(
            capi::PyImport_AddModule(c"BigWorld".as_ptr()),
            c"Base".as_ptr(),
        )
    }
}

/// Stamps the four `Base` script methods onto the live `BigWorld.Base`
/// type — the wiring the reference's `PY_METHOD` tables provide. The
/// scripts root for the cell-script probe comes from the host.
///
/// # Safety
///
/// The interpreter must be up and this thread must own the GIL.
pub unsafe fn install_base_methods() -> Result<(), String> {
    use python_host::capi;
    // SAFETY: the interpreter is up on this thread.
    unsafe {
        let base = base_type();
        if base.is_null() {
            return Err("BigWorld.Base is not registered".to_string());
        }
        let base = PyRef::borrowed(base);
        // `PyCFunction_New` stores the `PyMethodDef` *pointer* — the table
        // must outlive every method object, the `static PyMethodDef[]`
        // contract of the reference's `PY_METHOD` tables. The leaked slice
        // is the program-lifetime stand-in.
        let defs: Box<[capi::PyMethodDef]> = [
            c"createInNewSpace",
            c"registerGlobally",
            c"destroyCellEntity",
            c"destroy",
        ]
        .into_iter()
        .zip([
            base_create_in_new_space as capi::PyCFunction,
            base_register_globally as capi::PyCFunction,
            base_destroy_cell_entity as capi::PyCFunction,
            base_destroy_method as capi::PyCFunction,
        ])
        .map(|(name, handler)| capi::PyMethodDef {
            ml_name: name.as_ptr(),
            ml_meth: Some(handler),
            ml_flags: capi::METH_VARARGS,
            ml_doc: std::ptr::null(),
        })
        .collect::<Vec<_>>()
        .into_boxed_slice();
        let defs = Box::leak(defs);
        for def in defs.iter() {
            // The three C calls cannot fail on these inputs — the table is
            // static, the wrapper wraps a live function, and the type is a
            // writable heap type — so the calls run unchecked, the way the
            // reference's static `PY_METHOD` tables do.
            let function = capi::PyCFunction_New(
                def as *const capi::PyMethodDef as *mut capi::PyMethodDef,
                std::ptr::null_mut(),
            );
            let wrapper = capi::PyInstanceMethod_New(function);
            capi::dec_ref(function);
            capi::PyObject_SetAttrString(base.as_ptr(), def.ml_name, wrapper);
            capi::dec_ref(wrapper);
        }
    }
    Ok(())
}

/// Resolves the base id from the instance in `args[0]`, the
/// `PyInstanceMethod` binding of the stamped methods.
///
/// # Safety
///
/// The interpreter must be up and `args` a tuple whose first item is the
/// bound instance.
unsafe fn base_id_from_args(args: *mut python_host::capi::PyObject) -> Option<EntityId> {
    use python_host::capi;
    // SAFETY: the interpreter is up and the tuple is live. The
    // `PyInstanceMethod` binding always delivers the bound instance at
    // `args[0]` — the same unconditional `self` the reference's
    // `PY_METHOD` handlers read — so the tuple is indexed directly.
    unsafe {
        let instance = capi::PyTuple_GetItem(args, 0);
        with_host(|host| host.base_of_ptr(instance as usize))
    }
}

/// Raises `ValueError`/`TypeError` with the message and returns null — the
/// shared tail of every refused script method.
///
/// # Safety
///
/// The interpreter must be up.
unsafe fn raise(
    type_: *mut python_host::capi::PyObject,
    message: &str,
) -> *mut python_host::capi::PyObject {
    // SAFETY: the interpreter is up.
    unsafe {
        let text = std::ffi::CString::new(message).expect("an error message with no NUL");
        python_host::capi::PyErr_SetString(type_, text.as_ptr());
    }
    std::ptr::null_mut()
}

/// `Base.createInNewSpace([shouldPreferThisMachine])`
/// (`base.cpp:3548-3600`): the optional bool, the prepare ladder, the
/// request record.
///
/// # Safety
///
/// The interpreter must be up; `self_`/`args` are the C call's.
unsafe extern "C" fn base_create_in_new_space(
    self_: *mut python_host::capi::PyObject,
    args: *mut python_host::capi::PyObject,
) -> *mut python_host::capi::PyObject {
    use python_host::capi;
    let _ = self_;
    // SAFETY: the interpreter is up and the arguments are the C call's.
    unsafe {
        // The instance-method binding puts the instance at `args[0]`; the
        // script's own arguments start at `args[1]` — the reference's
        // `|O` arity (`base.cpp:3561`) is over what is left.
        let size = capi::PyTuple_Size(args);
        let mut prefer = false;
        if size > 2 {
            // The classic `PyArg_ParseTupleAndKeywords` text: the noun is
            // keyed on the maximum the format allows — 1, so always
            // singular — while the count is what the call supplied.
            let given = size - 1;
            return raise(
                capi::PyExc_TypeError,
                &format!("Base.createEntityInNewSpace() takes at most 1 argument ({given} given)"),
            );
        }
        if size == 2 {
            let value = capi::PyTuple_GetItem(args, 1);
            prefer = capi::PyObject_IsTrue(value) == 1;
        }
        let Some(id) = base_id_from_args(args) else {
            return raise(
                capi::PyExc_ValueError,
                "Base.createEntityInNewSpace: the base is unknown",
            );
        };
        let outcome = with_host(|host| host.create_in_new_space(id, prefer));
        match outcome {
            Ok(()) => capi::py_none(),
            Err(message) => {
                with_host(|host| host.script_errors.push(message.clone()));
                let prefix = "Base.createEntityInNewSpace: ";
                if message.ends_with("has no cell script.") {
                    raise(capi::PyExc_AttributeError, &message)
                } else {
                    let _ = prefix;
                    raise(capi::PyExc_ValueError, &message)
                }
            }
        }
    }
}

/// `Base.registerGlobally(key, callback)` (`base.cpp:4225-4273`): the
/// callable check, the key's string form, the pending record.
///
/// # Safety
///
/// The interpreter must be up; `self_`/`args` are the C call's.
unsafe extern "C" fn base_register_globally(
    self_: *mut python_host::capi::PyObject,
    args: *mut python_host::capi::PyObject,
) -> *mut python_host::capi::PyObject {
    use python_host::capi;
    let _ = self_;
    // SAFETY: the interpreter is up and the arguments are the C call's.
    unsafe {
        if capi::PyTuple_Size(args) != 3 {
            return raise(
                capi::PyExc_TypeError,
                "registerGlobally() takes exactly 2 arguments",
            );
        }
        let key_object = capi::PyTuple_GetItem(args, 1);
        let callback = capi::PyTuple_GetItem(args, 2);
        if capi::PyCallable_Check(callback) == 0 {
            return raise(
                capi::PyExc_TypeError,
                "The second argument must be callable",
            );
        }
        let rendered = capi::PyObject_Str(key_object);
        if rendered.is_null() {
            capi::clear_error();
            return raise(
                capi::PyExc_TypeError,
                "Unable to convert key to a valid string",
            );
        }
        let key = capi::to_string(rendered);
        capi::dec_ref(rendered);
        let Some(id) = base_id_from_args(args) else {
            return raise(
                capi::PyExc_ValueError,
                "Base.registerGlobally: the base is unknown",
            );
        };
        let owned = PyRef::borrowed(callback).clone();
        let _ = with_host(|host| host.register_globally(id, &key, owned));
        capi::py_none()
    }
}

/// `Base.destroyCellEntity()` (`base.cpp:4037-4070`): the loud ladder and
/// the idempotence guard.
///
/// # Safety
///
/// The interpreter must be up; `self_`/`args` are the C call's.
unsafe extern "C" fn base_destroy_cell_entity(
    self_: *mut python_host::capi::PyObject,
    args: *mut python_host::capi::PyObject,
) -> *mut python_host::capi::PyObject {
    use python_host::capi;
    let _ = self_;
    // SAFETY: the interpreter is up and the arguments are the C call's.
    unsafe {
        if capi::PyTuple_Size(args) > 1 {
            return raise(
                capi::PyExc_TypeError,
                "Base.destroyCellEntity() accepts no arguments",
            );
        }
        let Some(id) = base_id_from_args(args) else {
            return raise(
                capi::PyExc_ValueError,
                "Base.destroyCellEntity: the base is unknown",
            );
        };
        let outcome = with_host(|host| host.destroy_cell_entity(id));
        match outcome {
            Ok(()) => capi::py_none(),
            Err(message) => {
                with_host(|host| host.script_errors.push(message.clone()));
                raise(capi::PyExc_ValueError, &message)
            }
        }
    }
}

/// `Base.destroy()` (`base.cpp:3975-3995` + `base.cpp:1488-1560`): the
/// guards and the teardown sequence.
///
/// # Safety
///
/// The interpreter must be up; `self_`/`args` are the C call's.
unsafe extern "C" fn base_destroy_method(
    self_: *mut python_host::capi::PyObject,
    args: *mut python_host::capi::PyObject,
) -> *mut python_host::capi::PyObject {
    use python_host::capi;
    let _ = self_;
    // SAFETY: the interpreter is up and the arguments are the C call's.
    unsafe {
        if capi::PyTuple_Size(args) != 1 {
            return raise(capi::PyExc_TypeError, "Only expecting keyword arguments");
        }
        let Some(id) = base_id_from_args(args) else {
            return raise(capi::PyExc_ValueError, "Base.destroy: the base is unknown");
        };
        let outcome = host_destroy_base(id, false, false, false);
        match outcome {
            Ok(_) => capi::py_none(),
            Err(message) => {
                with_host(|host| host.script_errors.push(message.clone()));
                raise(capi::PyExc_ValueError, &message)
            }
        }
    }
}

/// Calls the bound `onRegistered`-shaped callback with one bool, the
/// `MakeGlobalHandler` tail.
///
/// # Safety
///
/// The interpreter must be up and `callback` a live callable.
unsafe fn call_bool_callback(callback: *mut python_host::capi::PyObject, succeeded: bool) {
    use python_host::capi;
    // SAFETY: the interpreter is up and the callback is live.
    unsafe {
        let argument = if succeeded {
            capi::py_true()
        } else {
            capi::py_false()
        };
        let tuple = capi::PyTuple_New(1);
        capi::PyTuple_SetItem(tuple, 0, argument);
        let result = capi::PyObject_CallObject(callback, tuple);
        capi::dec_ref(tuple);
        if result.is_null() {
            // `handleException` (`global_bases.cpp:245`): the callback's
            // failure prints and clears, the reply message carries on.
            capi::PyErr_Print();
        } else {
            capi::dec_ref(result);
        }
    }
}

/// Calls the named method on the script instance, tolerating a missing
/// method the way `setCurrentCell`'s `PyErr_Clear` does.
///
/// # Safety
///
/// The interpreter must be up and `instance` a live object.
unsafe fn call_script_method(instance: *mut python_host::capi::PyObject, name: &str) {
    use python_host::capi;
    if instance.is_null() {
        return;
    }
    // SAFETY: the interpreter is up and the instance is live.
    unsafe {
        let encoded = std::ffi::CString::new(name).expect("a method name with no NUL");
        let method = capi::PyObject_GetAttrString(instance, encoded.as_ptr());
        if method.is_null() {
            capi::clear_error();
            return;
        }
        let tuple = capi::PyTuple_New(0);
        let result = capi::PyObject_CallObject(method, tuple);
        capi::dec_ref(tuple);
        capi::dec_ref(method);
        if result.is_null() {
            // `Script::handleException`: the script's failure prints and
            // clears, the drive carries on.
            capi::PyErr_Print();
        } else {
            capi::dec_ref(result);
        }
    }
}

/// Clears the script instance's `__dict__` — `discard`'s teardown
/// (`base.cpp:1597-1599`).
///
/// # Safety
///
/// The interpreter must be up and `instance` a live object.
unsafe fn clear_script_dict(instance: *mut python_host::capi::PyObject) {
    use python_host::capi;
    // SAFETY: the interpreter is up and the instance is live.
    unsafe {
        let dictionary = capi::PyObject_GetAttrString(instance, c"__dict__".as_ptr());
        if dictionary.is_null() {
            capi::clear_error();
            return;
        }
        capi::PyDict_Clear(dictionary);
        capi::dec_ref(dictionary);
    }
}

/// The engine's construction of a base script instance
/// (`newEntityBase`, `baseapp/entity_type.cpp:560-583`, plus the
/// `tp_init` dispatch of `base.cpp:743-748`): allocate on the script
/// class without ever passing `tp_new`, register the record, then run the
/// script's `__init__` through the type's init slot with empty arguments.
/// A failing `__init__` prints and carries on, the reference's
/// `PyErr_Print` arm.
///
/// # Safety
///
/// The interpreter must be up, `class` a subclass of `BigWorld.Base`, and
/// this thread must own the GIL.
pub unsafe fn construct_base(class: &PyRef) -> Result<EntityId, String> {
    use python_host::binding;
    use python_host::capi;
    // SAFETY: the interpreter is up and the class is live.
    unsafe {
        let Some(instance) = binding::instantiate(class) else {
            return Err("the base script instance could not be allocated".to_string());
        };
        let name = capi::type_name(class.as_ptr().cast::<capi::PyTypeObject>());
        // The script class carries the module prefix the port's registered
        // types do ("BigWorld.Space"); the probe key is the bare tail.
        let bare = name.rsplit('.').next().unwrap_or(name.as_str()).to_string();
        let id = with_host(|host| {
            let id = host.allocate_id();
            host.register_script(id, &bare, instance.clone());
            id
        });
        let init = capi::PyType_GetSlot(
            class.as_ptr().cast::<capi::PyTypeObject>(),
            capi::Py_tp_init,
        );
        eprintln!("DEBUG tp_init slot ptr: {:?}", init);
        if init.is_null() {
            return Err("the script class has no init slot".to_string());
        }
        let init: unsafe extern "C" fn(
            *mut capi::PyObject,
            *mut capi::PyObject,
            *mut capi::PyObject,
        ) -> std::os::raw::c_int = std::mem::transmute(init);
        let arguments = capi::PyTuple_New(0);
        let result = init(instance.as_ptr(), arguments, std::ptr::null_mut());
        eprintln!("DEBUG tp_init call result: {result}");
        if result == -1 {
            capi::PyErr_Print();
        }
        capi::dec_ref(arguments);
        Ok(id)
    }
}

/// `EntityType::importEntityClass` (`baseapp/entity_type.cpp:81-131`):
/// import the module, read the class of the same name, enforce the
/// subclass rule against `base`. The `IGameDelegate` fallback classes are
/// the def wiring's face and stay out — a failed import is the hard error.
///
/// # Safety
///
/// The interpreter must be up and this thread must own the GIL.
pub unsafe fn import_entity_class(name: &str, base: &PyRef) -> Result<PyRef, String> {
    use python_host::capi;
    // SAFETY: the interpreter is up on this thread.
    unsafe {
        let encoded =
            std::ffi::CString::new(name).map_err(|_| format!("{name}: not a module name"))?;
        let module = capi::PyImport_ImportModule(encoded.as_ptr());
        if module.is_null() {
            capi::clear_error();
            return Err(format!(
                "EntityType::importEntityClass: Could not load module {name}"
            ));
        }
        let module = PyRef::from_owned(module).expect("a non-null module");
        let class = capi::PyObject_GetAttrString(module.as_ptr(), encoded.as_ptr());
        if class.is_null() {
            capi::clear_error();
            return Err(format!(
                "EntityType::importEntityClass: Could not find class {name}"
            ));
        }
        if capi::PyType_IsSubtype(
            class.cast::<capi::PyTypeObject>(),
            base.as_ptr().cast::<capi::PyTypeObject>(),
        ) == 0
        {
            capi::dec_ref(class);
            // `tp_name` is already the qualified name — `BigWorld.Base` —
            // so the text quotes it whole and prefixes nothing.
            let base_name = capi::type_name(base.as_ptr().cast::<capi::PyTypeObject>());
            return Err(format!(
                "EntityType::init: {name} should be derived from {base_name}"
            ));
        }
        PyRef::from_owned(class).ok_or_else(|| format!("{name}: the class vanished"))
    }
}

/// The cell-side construction (`entity_type.cpp:133-152` allocate, then
/// `callScriptInit`, `entity.cpp:1916-1933`): allocate on the script class
/// and dispatch `__init__` by name with no arguments — the cell side
/// bypasses the init slot, unlike the base side.
///
/// # Safety
///
/// The interpreter must be up, `class` a subclass of `BigWorld.Entity`,
/// and this thread must own the GIL.
pub unsafe fn construct_cell(class: &PyRef) -> Result<PyRef, String> {
    use python_host::binding;
    use python_host::capi;
    // SAFETY: the interpreter is up and the class is live.
    unsafe {
        let Some(instance) = binding::instantiate(class) else {
            return Err("the cell script instance could not be allocated".to_string());
        };
        let encoded = std::ffi::CString::new("__init__").expect("a fixed name");
        let init = capi::PyObject_GetAttrString(instance.as_ptr(), encoded.as_ptr());
        if init.is_null() {
            capi::clear_error();
            return Err("the cell script class has no __init__".to_string());
        }
        let arguments = capi::PyTuple_New(0);
        let outcome = capi::PyObject_CallObject(init, arguments);
        capi::dec_ref(arguments);
        capi::dec_ref(init);
        // A failed `__init__` prints and carries on — the same
        // `PyErr_Print` tail the base constructor gives the slot call —
        // and the call's own result (always `None` for `__init__`, but a
        // live reference either way) is dropped, not kept.
        if outcome.is_null() {
            capi::PyErr_Print();
        }
        drop(PyRef::from_owned(outcome));
        Ok(instance)
    }
}

// ---------------------------------------------------------------------------
// Tests: the pure transitions first, then the interpreter scenarios -- one
// interpreter per process, so each scenario runs as its own process of the
// test binary, the harness every python-host module follows.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn flags() -> BaseFlags {
        BaseFlags::default()
    }

    /// The five rungs of `checkAssociatedCellEntity`, each with its exact
    /// `ValueError` text (`base.cpp:3717-3760`).
    #[test]
    fn the_cell_entity_ladder_reports_each_rung_verbatim() {
        let mut f = flags();
        assert_eq!(
            check_associated_cell_entity(&f, true),
            CellEntityCheck::NoCell
        );
        assert_eq!(
            associated_cell_entity_error(CellEntityCheck::NoCell, 7, "P: "),
            "P: Base 7 does not have an associated cell entity."
        );

        f.has_cell = true;
        assert_eq!(
            check_associated_cell_entity(&f, false),
            CellEntityCheck::AlreadyHasCell
        );
        assert_eq!(
            associated_cell_entity_error(CellEntityCheck::AlreadyHasCell, 7, "P: "),
            "P: Base 7 already has an associated cell entity."
        );

        f.has_cell = false;
        f.is_get_cell_pending = true;
        assert_eq!(
            check_associated_cell_entity(&f, true),
            CellEntityCheck::GetPending
        );
        assert_eq!(
            associated_cell_entity_error(CellEntityCheck::GetPending, 7, "P: "),
            "P: Base 7 is waiting for contact from its created associated cell entity."
        );

        f.is_get_cell_pending = false;
        f.is_create_cell_pending = true;
        assert_eq!(
            check_associated_cell_entity(&f, true),
            CellEntityCheck::CreatePending
        );
        assert_eq!(
            associated_cell_entity_error(CellEntityCheck::CreatePending, 7, "P: "),
            "P: Base 7 is in the process of creating an associated cell entity."
        );

        f.is_create_cell_pending = false;
        f.is_destroyed = true;
        assert_eq!(
            check_associated_cell_entity(&f, true),
            CellEntityCheck::Destroyed
        );
        assert_eq!(
            associated_cell_entity_error(CellEntityCheck::Destroyed, 7, "P: "),
            "P: Base 7 is destroyed"
        );
    }

    /// `prepareForCellCreate` refuses a second create and a cell-less type,
    /// then sets the two pending flags together (`base.cpp:3269-3298`).
    #[test]
    fn prepare_for_cell_create_refuses_seconds_and_cellless_types() {
        let mut f = flags();
        assert!(matches!(
            prepare_for_cell_create(&mut f, false),
            Err(CellCreateError::NoCellScript)
        ));
        assert!(prepare_for_cell_create(&mut f, true).is_ok());
        assert!(f.is_create_cell_pending && f.is_get_cell_pending);
        // The second create is the "already has" rung -- the ladder runs
        // with `havingEntityGood = false`, so a pending create wins first.
        assert!(matches!(
            prepare_for_cell_create(&mut f, true),
            Err(CellCreateError::Check(CellEntityCheck::CreatePending))
        ));
    }

    /// `cellCreationResult` (`base.cpp:3757-3790`): destroyed inert, lost
    /// race inert, success clears the create pending only, failure also
    /// drops the get pending and fires the script.
    #[test]
    fn cell_creation_result_walks_its_four_arms() {
        let mut f = flags();
        f.is_destroyed = true;
        assert_eq!(
            cell_creation_result(&mut f, true),
            CellCreationOutcome::InertDestroyed
        );

        let mut f = flags();
        assert_eq!(
            cell_creation_result(&mut f, false),
            CellCreationOutcome::InertNotPending
        );

        let mut f = flags();
        f.is_create_cell_pending = true;
        f.is_get_cell_pending = true;
        assert_eq!(
            cell_creation_result(&mut f, true),
            CellCreationOutcome::Cleared
        );
        assert!(!f.is_create_cell_pending && f.is_get_cell_pending);

        let mut f = flags();
        f.is_create_cell_pending = true;
        f.is_get_cell_pending = true;
        assert_eq!(
            cell_creation_result(&mut f, false),
            CellCreationOutcome::FailedCallsScript
        );
        assert!(!f.is_create_cell_pending && !f.is_get_cell_pending);
    }

    /// The `setCurrentCell` transition (`base.cpp:1028-1044`): no change
    /// fires nothing, a change clears the pendings and names the callback.
    #[test]
    fn set_current_cell_fires_only_on_a_change() {
        let mut f = flags();
        assert_eq!(set_current_cell(&mut f, 3, false), None);

        f.is_create_cell_pending = true;
        f.is_get_cell_pending = true;
        assert_eq!(set_current_cell(&mut f, 3, true), Some(true));
        assert!(f.has_cell && !f.is_create_cell_pending && !f.is_get_cell_pending);
        assert_eq!(f.space_id, 3);

        assert_eq!(set_current_cell(&mut f, 3, true), None);
        assert_eq!(set_current_cell(&mut f, 3, false), Some(false));
        assert!(!f.has_cell);
    }

    /// The `py_destroy` guards (`base.cpp:3984-3995`) and the teardown
    /// step order (`base.cpp:1488-1560`), including the `hasWrittenToDB`
    /// gate and the destroyed no-op.
    #[test]
    fn destroy_walks_its_sequence_once() {
        let mut f = flags();
        f.has_cell = true;
        assert_eq!(check_destroy_allowed(&f), Err(DestroyError::StillHasCell));

        f.has_cell = false;
        f.is_get_cell_pending = true;
        assert_eq!(check_destroy_allowed(&f), Err(DestroyError::StillHasCell));

        f.is_get_cell_pending = false;
        f.is_destroyed = true;
        assert_eq!(
            check_destroy_allowed(&f),
            Err(DestroyError::AlreadyDestroyed)
        );

        // A fresh base: the write step joins only behind `hasWrittenToDB`.
        let mut f = flags();
        let steps = destroy_steps(&mut f, false, false, false);
        assert_eq!(
            steps,
            vec![
                DestroyStep::CallOnDestroy,
                DestroyStep::StopBackup,
                DestroyStep::DeregisterGlobals,
                DestroyStep::Discard,
            ]
        );
        assert!(f.is_destroyed);

        // A destroyed base records nothing the second time.
        assert!(destroy_steps(&mut f, false, false, false).is_empty());

        // A base that wrote to the DB carries the write step.
        let mut f = flags();
        f.has_written_to_db = true;
        let steps = destroy_steps(&mut f, true, false, true);
        assert!(steps.contains(&DestroyStep::WriteDb));
        assert_eq!(steps.first(), Some(&DestroyStep::CallOnDestroy));
        assert_eq!(steps.last(), Some(&DestroyStep::Discard));
    }

    /// The host's `createInNewSpace` record: without a cell script in the
    /// res root the probe refuses with the `AttributeError` arm and no
    /// request lands.
    #[test]
    fn the_host_records_the_create_request_and_probes_by_type_name() {
        let mut host = ScriptHost::new();
        host.set_scripts_root(PathBuf::from("/nonexistent-root"));
        host.flags.insert(4, BaseFlags::default());
        host.type_names.insert(4, "Space".to_string());
        assert_eq!(
            host.create_in_new_space(4, false),
            Err("Base.createEntityInNewSpace: Base 4 has no cell script.".to_string())
        );
        assert!(host.outbound.is_empty());
    }

    /// The plain-`Err` arms of the drives: unknown ids, the pass-through
    /// ladder, the failure signal, and every `destroy` guard — all on a
    /// local host, no interpreter (none of these run Python).
    #[test]
    fn the_host_drive_guards_refuse_without_a_record() {
        let mut host = ScriptHost::new();
        host.set_scripts_root(PathBuf::from("/nonexistent-root"));
        // The ladder's OK arm passes through to the loud text.
        let f = BaseFlags {
            has_cell: true,
            ..BaseFlags::default()
        };
        assert_eq!(
            associated_cell_entity_error(check_associated_cell_entity(&f, true), 7, "P: "),
            ""
        );
        // Unknown ids refuse with the destroyed text.
        assert_eq!(
            host.create_in_new_space(9, false),
            Err("Base.createEntityInNewSpace: Base 9 is destroyed".to_string())
        );
        assert_eq!(
            host.destroy_cell_entity(9),
            Err("Base.destroyCellEntity: Base 9 is destroyed".to_string())
        );
        assert_eq!(
            host.destroy_base(9, false, false, false),
            Err("Base entity already destroyed".to_string())
        );
        host.on_cell_creation_result(9, true);
        assert!(host.script_signals.is_empty());
        // The existing-cell ladder rides through `create_in_new_space`.
        host.flags.insert(
            5,
            BaseFlags {
                has_cell: true,
                ..BaseFlags::default()
            },
        );
        host.type_names.insert(5, "Avatar".to_string());
        assert_eq!(
            host.create_in_new_space(5, false),
            Err(
                "Base.createEntityInNewSpace: Base 5 already has an associated cell entity."
                    .to_string()
            )
        );
        // The failure arm pushes the script signal — only with the create
        // pending up; without it the reply is the inert arm.
        host.flags.insert(
            6,
            BaseFlags {
                is_create_cell_pending: true,
                ..BaseFlags::default()
            },
        );
        host.on_cell_creation_result(6, false);
        assert_eq!(
            host.script_signals,
            vec![ScriptSignal::OnCreateCellFailure(6)]
        );
        host.on_cell_creation_result(6, false);
        assert_eq!(host.script_signals.len(), 1, "the second reply is inert");
        // `destroy` refuses while a cell exists or once destroyed.
        host.flags.insert(
            7,
            BaseFlags {
                has_cell: true,
                ..BaseFlags::default()
            },
        );
        assert_eq!(
            host.destroy_base(7, false, false, false),
            Err("Still has cell entity. Use Base.destroyCellEntity".to_string())
        );
        host.flags.insert(
            8,
            BaseFlags {
                is_destroyed: true,
                ..BaseFlags::default()
            },
        );
        assert_eq!(
            host.destroy_base(8, false, false, false),
            Err("Base entity already destroyed".to_string())
        );
    }

    /// The thread-local drive guards: unknown keys and ids answer `false`
    /// or nothing, a destroyed base refuses, a drive in flight is the
    /// `inDestroy_` no-op, and a null callback is the silent arm.
    #[test]
    fn the_thread_local_drives_ignore_unknown_records() {
        assert!(!host_on_register_reply("nope", 1));
        host_set_current_cell(3, 1, true);
        assert_eq!(
            host_destroy_base(3, false, false, false),
            Err("Base entity already destroyed".to_string())
        );
        // Seed one record directly: the destroyed flag refuses the drive,
        // the in-destroy latch is the no-op the reentrant script sees.
        with_host(|host| {
            host.flags.insert(
                3,
                BaseFlags {
                    is_destroyed: true,
                    ..BaseFlags::default()
                },
            );
        });
        assert_eq!(
            host_destroy_base(3, false, false, false),
            Err("Base entity already destroyed".to_string())
        );
        with_host(|host| {
            host.flags.insert(
                4,
                BaseFlags {
                    in_destroy: true,
                    ..BaseFlags::default()
                },
            );
        });
        assert_eq!(host_destroy_base(4, false, false, false), Ok(Vec::new()));
        // A null instance pointer is the silent arm — nothing dispatches.
        // SAFETY: no callback runs for a null pointer.
        unsafe { call_script_method(std::ptr::null_mut(), "onGetCell") };
        // Leave the thread-local records alone: the scenario harness runs
        // in subprocesses, but the pure probes here read a shared HOST.
        with_host(|host| host.flags.clear());
    }

    // -- interpreter scenarios ------------------------------------------

    /// Every scenario, each in its own process: the interpreter refuses a
    /// second init in one process, the same rule every harness follows.
    const SCENARIOS: &[&str] = &[
        "script_host::tests::scenario_the_space_bootstrap_records_its_requests",
        "script_host::tests::scenario_the_duplicate_registration_destroys_the_pending_cell",
        "script_host::tests::scenario_a_reply_after_destroy_still_fires_the_callback",
        "script_host::tests::scenario_the_cell_lifecycle_drives_the_script",
        "script_host::tests::scenario_destroy_without_a_cell_is_a_loud_error",
        "script_host::tests::scenario_destroy_refuses_while_a_cell_exists",
        "script_host::tests::scenario_a_non_callable_callback_is_refused",
        "script_host::tests::scenario_the_guard_constructs_on_the_cell_role",
        "script_host::tests::scenario_the_wrong_arity_is_the_exact_type_error",
        "script_host::tests::scenario_an_unregistered_instance_finds_no_host_record",
        "script_host::tests::scenario_the_register_and_import_refusals",
        "script_host::tests::scenario_the_stamps_refuse_without_the_base_type",
        "script_host::tests::scenario_a_destroy_from_on_destroy_is_the_no_op",
        "script_host::tests::scenario_a_failed_registration_reply_carries_false",
        "script_host::tests::scenario_a_failing_cell_init_prints_and_carries_on",
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
        let output = std::process::Command::new(&harness)
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

    /// `Interpreter::init` around one body, on the fixture's own role
    /// scripts -- unchanged text.
    fn interpreter_scenario(component: &str, body: impl FnOnce()) {
        let root = temp_resource_root();
        let role = demo_scripts_dir(component);
        let mut paths = python_host::import_paths::ImportPaths::default();
        paths.add_res_path(
            &[&root.to_string_lossy()],
            python_host::import_paths::COMMON_PATH,
        );
        paths.add_non_res_path(&role.to_string_lossy());
        let interpreter = python_host::Interpreter::init(&python_host::InitConfig {
            component: String::from(component),
            import_paths: paths,
            ..python_host::InitConfig::default()
        })
        .expect("Script::init");
        // The cell-script probe reads the fixture res root.
        with_host(|host| host.set_scripts_root(fixture_res_root()));
        body();
        interpreter.fini();
    }

    /// A throw-away resource root with the one module the interpreter's
    /// startup always imports: `bw_site`.
    fn temp_resource_root() -> PathBuf {
        let root =
            std::env::temp_dir().join(format!("boundless-script-host-{}", std::process::id()));
        std::fs::remove_dir_all(&root).ok();
        std::fs::create_dir_all(root.join("scripts/common/Lib")).expect("build the script tree");
        std::fs::write(
            root.join("scripts/common/bw_site.py"),
            "import sys\nsys.bw_site_imported = True\n",
        )
        .expect("write bw_site");
        root
    }

    /// The generated fantasydemo fixture res root, in the repo.
    fn fixture_res_root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../docs/review/fixtures/fantasydemo_def/res")
    }

    /// The generated fantasydemo fixture scripts of one role, in the repo.
    fn demo_scripts_dir(component: &str) -> PathBuf {
        fixture_res_root().join("scripts").join(component)
    }

    /// `BigWorld.Base`/`BigWorld.Entity`, borrowed for the call's scope.
    ///
    /// # Safety
    ///
    /// The interpreter must be up.
    unsafe fn bigworld_class(name: &str) -> PyRef {
        // SAFETY: the interpreter is up on this thread.
        unsafe {
            let module = python_host::capi::PyImport_AddModule(c"BigWorld".as_ptr());
            assert!(!module.is_null(), "the BigWorld module is up");
            let encoded = std::ffi::CString::new(name).expect("a fixed name");
            let class = python_host::capi::PyObject_GetAttrString(module, encoded.as_ptr());
            assert!(!class.is_null(), "BigWorld.{name} is registered");
            PyRef::from_owned(class).expect("a non-null class")
        }
    }

    /// Runs `source` and reads the `__probe` it assigns back as a string.
    ///
    /// # Safety
    ///
    /// Only called from a scenario body, while the interpreter is up on
    /// the thread that owns the GIL.
    unsafe fn probe(source: &str) -> String {
        // SAFETY: only called from a scenario body, while the interpreter
        // is up on the thread that owns the GIL.
        unsafe {
            let Ok(encoded) = std::ffi::CString::new(source) else {
                panic!("probe source has no embedded NUL");
            };
            assert!(
                python_host::capi::PyRun_SimpleString(encoded.as_ptr()) == 0,
                "script must run: {source}"
            );
            let main = python_host::capi::PyImport_AddModule(c"__main__".as_ptr());
            let dictionary = python_host::capi::PyModule_GetDict(main);
            let value = python_host::capi::PyDict_GetItemString(dictionary, c"__probe".as_ptr());
            let text = python_host::capi::PyObject_Str(value);
            let rendered = python_host::capi::to_string(text);
            python_host::capi::dec_ref(text);
            rendered
        }
    }

    /// Reads the `EVENTS` list the recorder class appends to, as bools.
    ///
    /// # Safety
    ///
    /// The interpreter must be up.
    unsafe fn probe_events() -> Vec<bool> {
        // The throwaway res root carries no stdlib, so the probe is the
        // plain `repr` of the list -- `[True]`/`[True, False]`/`[]`.
        let rendered = unsafe { probe("__probe = str( EVENTS )") };
        rendered
            .trim()
            .trim_start_matches('[')
            .trim_end_matches(']')
            .split(',')
            .filter(|item| !item.trim().is_empty())
            .map(|item| match item.trim() {
                "True" => true,
                "False" => false,
                other => panic!("unexpected event item: {other}"),
            })
            .collect()
    }

    /// Defines a minimal base-script class `T` in `__main__`, recording
    /// `onRegistered` results into the `EVENTS` list, and hands the class
    /// back for the engine's construction.
    ///
    /// # Safety
    ///
    /// The interpreter must be up.
    unsafe fn define_recorder_class() -> PyRef {
        // SAFETY: the interpreter is up on this thread.
        unsafe {
            assert!(
                python_host::capi::PyRun_SimpleString(
                    c"import BigWorld\n\
                      EVENTS = []\n\
                      class T(BigWorld.Base):\n\
                      \x20   def __init__( self ):\n\
                      \x20       BigWorld.Base.__init__( self )\n\
                      \x20   def onRegistered( self, succeeded ):\n\
                      \x20       EVENTS.append( succeeded )\n"
                        .as_ptr()
                ) == 0,
                "the recorder class must define"
            );
            let main = python_host::capi::PyImport_AddModule(c"__main__".as_ptr());
            let dictionary = python_host::capi::PyModule_GetDict(main);
            let class = python_host::capi::PyDict_GetItemString(dictionary, c"T".as_ptr());
            assert!(!class.is_null(), "the class is in __main__");
            PyRef::borrowed(class).clone()
        }
    }

    /// Hands the engine's instance of `id` to `__main__` as `name`, so the
    /// probe scripts reach the engine-constructed object -- the same face
    /// the reference's globals give the debugger.
    ///
    /// # Safety
    ///
    /// The interpreter must be up.
    unsafe fn expose_instance(id: EntityId, name: &str) {
        // SAFETY: the interpreter is up on the scenario thread.
        unsafe {
            let instance = with_host(|host| host.scripts.get(&id).cloned());
            let instance = instance.expect("the base is registered");
            let main = python_host::capi::PyImport_AddModule(c"__main__".as_ptr());
            let encoded = std::ffi::CString::new(name).expect("a fixed name");
            assert!(
                python_host::capi::PyObject_SetAttrString(
                    main,
                    encoded.as_ptr(),
                    instance.as_ptr()
                ) == 0,
                "the instance is exposed"
            );
        }
    }

    /// The fixture's own `Space.py` bootstrap, run by the engine: the two
    /// requests land in the outbox in the script's order, with the pending
    /// flags up and the registration awaiting its reply.
    #[test]
    #[ignore]
    fn scenario_the_space_bootstrap_records_its_requests() {
        interpreter_scenario("base", || {
            // SAFETY: the scenario body, interpreter up.
            unsafe {
                assert!(install_base_methods().is_ok(), "the Base methods stamp");
                let base = bigworld_class("Base");
                let space = import_entity_class("Space", &base).expect("Space imports");
                let id = construct_base(&space).expect("the engine constructs Space");
                with_host(|host| {
                    assert_eq!(
                        host.outbound,
                        vec![
                            BaseOutbound::CreateEntityInNewSpace {
                                base: id,
                                should_prefer_this_machine: false
                            },
                            BaseOutbound::RegisterBaseGlobally {
                                base: id,
                                key: "DefaultSpace".to_string()
                            },
                        ]
                    );
                    let flags = &host.flags[&id];
                    assert!(flags.is_create_cell_pending && flags.is_get_cell_pending);
                    assert!(!flags.has_cell);
                    assert_eq!(host.pending_registrations.len(), 1);
                    assert_eq!(host.pending_registrations[0].key, "DefaultSpace");
                });
            }
        });
    }

    /// The duplicate-registration path: the callback gets `False`, and the
    /// script's `onRegistered(False)` calls `destroyCellEntity()` — which
    /// goes *through*: `prepareForCellCreate` raised both pending flags, so
    /// `py_destroyCellEntity`'s get-pending bypass
    /// (`base.cpp:4038-4041`) lets the destroy request land and the
    /// `isDestroyCellPending_` latch rise; a second call is the silent
    /// idempotence (`base.cpp:4050`).
    #[test]
    #[ignore]
    fn scenario_the_duplicate_registration_destroys_the_pending_cell() {
        interpreter_scenario("base", || {
            // SAFETY: the scenario body, interpreter up.
            unsafe {
                assert!(install_base_methods().is_ok());
                let base = bigworld_class("Base");
                let space = import_entity_class("Space", &base).expect("Space imports");
                let id = construct_base(&space).expect("the engine constructs Space");
                assert!(host_on_register_reply("DefaultSpace", 0));
                with_host(|host| {
                    // The failed reply does not enrol the base.
                    assert!(host.global_bases.get("DefaultSpace").is_none());
                    // The script's destroyCellEntity went through the bypass.
                    assert!(
                        host.outbound
                            .contains(&BaseOutbound::DestroyCellEntity { base: id })
                    );
                    let flags = &host.flags[&id];
                    assert!(flags.is_destroy_cell_pending);
                    // The idempotence: a second call records nothing.
                    let before = host.outbound.len();
                    assert!(host.destroy_cell_entity(id) == Ok(()));
                    assert_eq!(host.outbound.len(), before);
                });
            }
        });
    }

    /// `global_bases.cpp:240-242`: the reply handler holds the callback,
    /// not the base — a base destroyed in the meantime still fires it.
    #[test]
    #[ignore]
    fn scenario_a_reply_after_destroy_still_fires_the_callback() {
        interpreter_scenario("base", || {
            // SAFETY: the scenario body, interpreter up.
            unsafe {
                assert!(install_base_methods().is_ok());
                let class = define_recorder_class();
                let id = construct_base(&class).expect("the engine constructs T");
                expose_instance(id, "t");
                // The recorder never registers in `__init__`: the scenario
                // hands the bound method over by hand to pin the timing.
                let callback = with_host(|host| {
                    let instance = host.scripts.get(&id).expect("registered").clone();
                    let method = python_host::capi::PyObject_GetAttrString(
                        instance.as_ptr(),
                        c"onRegistered".as_ptr(),
                    );
                    assert!(!method.is_null(), "the bound method resolves");
                    PyRef::from_owned(method).expect("the bound method")
                });
                with_host(|host| {
                    host.register_globally(id, "K", callback)
                        .expect("the request records")
                });
                with_host(|host| {
                    host.destroy_base(id, false, false, false)
                        .expect("the base destroys")
                });
                assert!(host_on_register_reply("K", 1));
                assert_eq!(
                    probe_events(),
                    vec![true],
                    "the callback fired after the destroy"
                );
            }
        });
    }

    /// The full lifecycle on the fixture's Space: the create reply enrols
    /// the global base and answers the callback `True`, the cell contacts
    /// (`onGetCell` absent, silent), losing the cell runs `onLoseCell`
    /// whose `destroy()` drops the global registration and discards the
    /// base — the whole bootstrap, in reverse.
    #[test]
    #[ignore]
    fn scenario_the_cell_lifecycle_drives_the_script() {
        interpreter_scenario("base", || {
            // SAFETY: the scenario body, interpreter up.
            unsafe {
                assert!(install_base_methods().is_ok());
                let base = bigworld_class("Base");
                let space = import_entity_class("Space", &base).expect("Space imports");
                let id = construct_base(&space).expect("the engine constructs Space");
                assert!(host_on_register_reply("DefaultSpace", 1));
                with_host(|host| {
                    assert!(host.global_bases.get("DefaultSpace").is_some());
                });
                with_host(|host| host.on_cell_creation_result(id, true));
                // The cell contacts us: `onGetCell` is absent, silent.
                host_set_current_cell(id, 5, true);
                with_host(|host| {
                    assert!(host.script_signals.contains(&ScriptSignal::OnGetCell(id)));
                    let flags = &host.flags[&id];
                    assert!(flags.has_cell && flags.space_id == 5);
                    assert!(!flags.is_create_cell_pending && !flags.is_get_cell_pending);
                });
                // The cell goes away: `onLoseCell` -> `destroy()`.
                host_set_current_cell(id, 5, false);
                with_host(|host| {
                    let flags = &host.flags[&id];
                    assert!(flags.is_destroyed, "the script's destroy() ran");
                    assert!(host.global_bases.get("DefaultSpace").is_none());
                    assert!(
                        host.outbound
                            .contains(&BaseOutbound::DeregisterBaseGlobally {
                                key: "DefaultSpace".to_string()
                            })
                    );
                });
            }
        });
    }

    /// `destroyCellEntity` on a cell-less base is the loud `ValueError`
    /// (`base.cpp:3747-3750` through the `havingEntityGood = true` ladder).
    #[test]
    #[ignore]
    fn scenario_destroy_without_a_cell_is_a_loud_error() {
        interpreter_scenario("base", || {
            // SAFETY: the scenario body, interpreter up.
            unsafe {
                assert!(install_base_methods().is_ok());
                let class = define_recorder_class();
                let id = construct_base(&class).expect("the engine constructs T");
                expose_instance(id, "t");
                let answer = probe(
                    "try:\n\
                     \x20   t.destroyCellEntity()\n\
                     \x20   __probe = 'no error'\n\
                     except ValueError as error:\n\
                     \x20   __probe = str( error )",
                );
                assert_eq!(
                    answer,
                    "Base.destroyCellEntity: Base 1 does not have an associated cell entity."
                );
            }
        });
    }

    /// `destroy()` while a cell exists refuses with the destroy-cell hint
    /// (`base.cpp:3986-3991`).
    #[test]
    #[ignore]
    fn scenario_destroy_refuses_while_a_cell_exists() {
        interpreter_scenario("base", || {
            // SAFETY: the scenario body, interpreter up.
            unsafe {
                assert!(install_base_methods().is_ok());
                let class = define_recorder_class();
                let id = construct_base(&class).expect("the engine constructs T");
                expose_instance(id, "t");
                host_set_current_cell(id, 2, true);
                let answer = probe(
                    "try:\n\
                     \x20   t.destroy()\n\
                     \x20   __probe = 'no error'\n\
                     except ValueError as error:\n\
                     \x20   __probe = str( error )",
                );
                assert_eq!(answer, "Still has cell entity. Use Base.destroyCellEntity");
            }
        });
    }

    /// `registerGlobally` with a non-callable is the exact `TypeError`
    /// (`base.cpp:4245`).
    #[test]
    #[ignore]
    fn scenario_a_non_callable_callback_is_refused() {
        interpreter_scenario("base", || {
            // SAFETY: the scenario body, interpreter up.
            unsafe {
                assert!(install_base_methods().is_ok());
                let class = define_recorder_class();
                let id = construct_base(&class).expect("the engine constructs T");
                expose_instance(id, "t");
                let answer = probe(
                    "try:\n\
                     \x20   t.registerGlobally( 'K', 42 )\n\
                     \x20   __probe = 'no error'\n\
                     except TypeError as error:\n\
                     \x20   __probe = str( error )",
                );
                assert_eq!(answer, "The second argument must be callable");
            }
        });
    }

    /// The cell role: the fixture's Guard imports against
    /// `BigWorld.Entity`, the engine constructs it (the `__init__`-by-name
    /// dispatch, `entity.cpp:1916-1933`), the instance is live and
    /// mutable, and the fixture's cell `Space` constructs the same way.
    #[test]
    #[ignore]
    fn scenario_the_guard_constructs_on_the_cell_role() {
        interpreter_scenario("cell", || {
            // SAFETY: the scenario body, interpreter up.
            unsafe {
                let entity = bigworld_class("Entity");
                let guard = import_entity_class("Guard", &entity).expect("Guard imports");
                let instance = construct_cell(&guard).expect("the engine constructs Guard");
                // The instance is live and mutable: an attribute
                // round-trip through the C API proves it.
                python_host::capi::PyObject_SetAttrString(
                    instance.as_ptr(),
                    c"probe".as_ptr(),
                    python_host::capi::py_true(),
                );
                let value =
                    python_host::capi::PyObject_GetAttrString(instance.as_ptr(), c"probe".as_ptr());
                assert!(!value.is_null(), "the attribute round-trips");
                python_host::capi::dec_ref(value);
                // And the fixture's cell Space constructs unchanged too.
                let space_cell = import_entity_class("Space", &entity).expect("cell Space imports");
                construct_cell(&space_cell).expect("the engine constructs cell Space");
            }
        });
    }

    /// The arity refusals of every stamped method, each with the exact
    /// `TypeError` the reference's `PyArg_ParseTuple*` would raise —
    /// `createInNewSpace`'s `|O` text is CPython's own wording.
    #[test]
    #[ignore]
    fn scenario_the_wrong_arity_is_the_exact_type_error() {
        interpreter_scenario("base", || {
            // SAFETY: the scenario body, interpreter up.
            unsafe {
                assert!(install_base_methods().is_ok());
                let class = define_recorder_class();
                let id = construct_base(&class).expect("the engine constructs T");
                expose_instance(id, "t");
                let cases: &[(&str, &str)] = &[
                    (
                        "t.createInNewSpace( True, False )",
                        "Base.createEntityInNewSpace() takes at most 1 argument (2 given)",
                    ),
                    (
                        "t.registerGlobally( 'K' )",
                        "registerGlobally() takes exactly 2 arguments",
                    ),
                    (
                        "t.destroyCellEntity( 1 )",
                        "Base.destroyCellEntity() accepts no arguments",
                    ),
                    ("t.destroy( 1 )", "Only expecting keyword arguments"),
                ];
                for (call, expected) in cases {
                    let source = format!(
                        "try:\n\
                         \x20   {call}\n\
                         \x20   __probe = 'no error'\n\
                         except TypeError as error:\n\
                         \x20   __probe = str( error )"
                    );
                    assert_eq!(probe(&source), *expected, "arity text of {call}");
                }
            }
        });
    }

    /// A `BigWorld.Base` instance the engine did not register — allocated
    /// straight on the base class, no host record — finds no record in
    /// every stamped method, the "unknown base" `ValueError` of each.
    #[test]
    #[ignore]
    fn scenario_an_unregistered_instance_finds_no_host_record() {
        interpreter_scenario("base", || {
            // SAFETY: the scenario body, interpreter up.
            unsafe {
                assert!(install_base_methods().is_ok());
                let base = bigworld_class("Base");
                // `GenericAlloc` bypasses the refusing `tp_new`, the same
                // way the engine's own allocation does.
                let raw = python_host::binding::instantiate(&base).expect("the raw instance");
                python_host::capi::PyObject_SetAttrString(
                    python_host::capi::PyImport_AddModule(c"__main__".as_ptr()),
                    c"raw".as_ptr(),
                    raw.as_ptr(),
                );
                let cases: &[(&str, &str)] = &[
                    (
                        "raw.createInNewSpace()",
                        "Base.createEntityInNewSpace: the base is unknown",
                    ),
                    (
                        "raw.registerGlobally( 'K', print )",
                        "Base.registerGlobally: the base is unknown",
                    ),
                    (
                        "raw.destroyCellEntity()",
                        "Base.destroyCellEntity: the base is unknown",
                    ),
                    ("raw.destroy()", "Base.destroy: the base is unknown"),
                ];
                for (call, expected) in cases {
                    let source = format!(
                        "try:\n\
                         \x20   {call}\n\
                         \x20   __probe = 'no error'\n\
                         except ValueError as error:\n\
                         \x20   __probe = str( error )"
                    );
                    assert_eq!(probe(&source), *expected, "unknown-base text of {call}");
                }
            }
        });
    }

    /// The remaining refusals: an unstringable key, the preference flag
    /// riding through, the second create on the fixture's Space (the loud
    /// ladder through the handler), a module that will not load, and a
    /// class outside the hierarchy.
    #[test]
    #[ignore]
    fn scenario_the_register_and_import_refusals() {
        interpreter_scenario("base", || {
            // SAFETY: the scenario body, interpreter up.
            unsafe {
                assert!(install_base_methods().is_ok());
                let base = bigworld_class("Base");
                let class = define_recorder_class();
                let id = construct_base(&class).expect("the engine constructs T");
                expose_instance(id, "t");
                // The preference flag rides through even though the probe
                // then refuses: T has no cell script, the AttributeError arm.
                let answer = probe(
                    "try:\n\
                     \x20   t.createInNewSpace( True )\n\
                     \x20   __probe = 'no error'\n\
                     except AttributeError as error:\n\
                     \x20   __probe = str( error )",
                );
                assert_eq!(
                    answer,
                    "Base.createEntityInNewSpace: Base 1 has no cell script."
                );
                // An unstringable key refuses before the record.
                let answer = probe(
                    "class BadKey:\n\
                     \x20   def __str__( self ):\n\
                     \x20       raise RuntimeError( 'no key' )\n\
                     try:\n\
                     \x20   t.registerGlobally( BadKey(), print )\n\
                     \x20   __probe = 'no error'\n\
                     except TypeError as error:\n\
                     \x20   __probe = str( error )",
                );
                assert_eq!(answer, "Unable to convert key to a valid string");
                // The module that will not load, and the class outside the
                // hierarchy — the `importEntityClass` texts verbatim.
                let missing = import_entity_class("NoSuchModule", &base);
                assert_eq!(
                    missing,
                    Err(
                        "EntityType::importEntityClass: Could not load module NoSuchModule"
                            .to_string()
                    )
                );
                // A module whose class sits outside the hierarchy: the
                // temp root's `Lib` is on the import path.
                let common = temp_resource_root().join("scripts/common");
                std::fs::write(
                    common.join("stranger_mod.py"),
                    "class stranger_mod:\n\x20   pass\n",
                )
                .expect("write the stranger module");
                assert_eq!(
                    import_entity_class("stranger_mod", &base).map(|_| "imported"),
                    Err(
                        "EntityType::init: stranger_mod should be derived from BigWorld.Base"
                            .to_string()
                    )
                );
                // The second create on the fixture's Space is the loud
                // ladder, raised through the stamped handler.
                let space = import_entity_class("Space", &base).expect("Space imports");
                let space_id = construct_base(&space).expect("the engine constructs Space");
                expose_instance(space_id, "sp");
                let answer = probe(
                    "try:\n\
                     \x20   sp.createInNewSpace()\n\
                     \x20   __probe = 'no error'\n\
                     except ValueError as error:\n\
                     \x20   __probe = str( error )",
                );
                assert_eq!(
                    answer,
                    "Base.createEntityInNewSpace: Base 2 is in the process of creating an associated cell entity."
                );
            }
        });
    }

    /// Without the `Base` type the stamps refuse — the not-registered arm
    /// the module-null world would reach.
    #[test]
    #[ignore]
    fn scenario_the_stamps_refuse_without_the_base_type() {
        interpreter_scenario("base", || {
            // SAFETY: the scenario body, interpreter up.
            unsafe {
                probe("import BigWorld\ndelattr( BigWorld, 'Base' )");
                assert_eq!(
                    install_base_methods(),
                    Err("BigWorld.Base is not registered".to_string())
                );
            }
        });
    }

    /// The reference's `inDestroy_` no-op (`base.cpp:1490`): a `destroy()`
    /// from inside `onDestroy` finds the latch up, records nothing, and the
    /// outer drive completes with the full step list.
    #[test]
    #[ignore]
    fn scenario_a_destroy_from_on_destroy_is_the_no_op() {
        interpreter_scenario("base", || {
            // SAFETY: the scenario body, interpreter up.
            unsafe {
                assert!(install_base_methods().is_ok());
                assert!(
                    python_host::capi::PyRun_SimpleString(
                        c"import BigWorld\n\
                          EVENTS2 = []\n\
                          class R( BigWorld.Base ):\n\
                          \x20   def __init__( self ):\n\
                          \x20       BigWorld.Base.__init__( self )\n\
                          \x20   def onDestroy( self ):\n\
                          \x20       EVENTS2.append( 'enter' )\n\
                          \x20       self.destroy()\n\
                          \x20       EVENTS2.append( 'exit' )\n"
                            .as_ptr()
                    ) == 0,
                    "the reentrant class must define"
                );
                let main = python_host::capi::PyImport_AddModule(c"__main__".as_ptr());
                let dictionary = python_host::capi::PyModule_GetDict(main);
                let class = python_host::capi::PyDict_GetItemString(dictionary, c"R".as_ptr());
                let class = PyRef::borrowed(class).clone();
                let id = construct_base(&class).expect("the engine constructs R");
                let steps = host_destroy_base(id, false, false, false).expect("the base destroys");
                assert_eq!(
                    steps,
                    vec![
                        DestroyStep::CallOnDestroy,
                        DestroyStep::StopBackup,
                        DestroyStep::DeregisterGlobals,
                        DestroyStep::Discard,
                    ]
                );
                // The nested destroy was the no-op: one enter, one exit.
                let rendered = probe("__probe = str( EVENTS2 )");
                assert_eq!(rendered, "['enter', 'exit']");
                // And the destroyed base refuses the next drive.
                let again = host_destroy_base(id, false, false, false);
                assert_eq!(again, Err("Base entity already destroyed".to_string()));
            }
        });
    }

    /// The reply a failed registration carries — `False` — reaches the
    /// script's `onRegistered` and reads back from the `EVENTS` list.
    #[test]
    #[ignore]
    fn scenario_a_failed_registration_reply_carries_false() {
        interpreter_scenario("base", || {
            // SAFETY: the scenario body, interpreter up.
            unsafe {
                assert!(install_base_methods().is_ok());
                let class = define_recorder_class();
                let id = construct_base(&class).expect("the engine constructs T");
                expose_instance(id, "t");
                with_host(|host| {
                    let instance = host.scripts.get(&id).expect("registered").clone();
                    let method = python_host::capi::PyObject_GetAttrString(
                        instance.as_ptr(),
                        c"onRegistered".as_ptr(),
                    );
                    host.register_globally(id, "K", PyRef::from_owned(method).expect("method"))
                        .expect("the request records");
                });
                assert!(host_on_register_reply("K", 0));
                assert_eq!(probe_events(), vec![false], "the False event reads back");
            }
        });
    }

    /// A failing cell `__init__` prints and carries on (`PyErr_Print`,
    /// `base.cpp:748`); a missing `__init__` is the silent refusal.
    #[test]
    #[ignore]
    fn scenario_a_failing_cell_init_prints_and_carries_on() {
        interpreter_scenario("cell", || {
            // SAFETY: the scenario body, interpreter up.
            unsafe {
                assert!(
                    python_host::capi::PyRun_SimpleString(
                        c"import BigWorld\n\
                          class Bad( BigWorld.Entity ):\n\
                          \x20   def __init__( self ):\n\
                          \x20       raise RuntimeError( 'no init' )\n\
                          class Bare( BigWorld.Entity ):\n\
                          \x20   pass\n"
                            .as_ptr()
                    ) == 0,
                    "the cell classes must define"
                );
                let main = python_host::capi::PyImport_AddModule(c"__main__".as_ptr());
                let dictionary = python_host::capi::PyModule_GetDict(main);
                let bad = python_host::capi::PyDict_GetItemString(dictionary, c"Bad".as_ptr());
                let bad = PyRef::borrowed(bad).clone();
                construct_cell(&bad).expect("the engine carries on");
                let bare = python_host::capi::PyDict_GetItemString(dictionary, c"Bare".as_ptr());
                let bare = PyRef::borrowed(bare).clone();
                // A class with no `__init__` of its own inherits
                // `object.__init__`, so the lookup succeeds and the bare
                // entity constructs fine.
                let _bare = construct_cell(&bare).expect("the bare entity constructs");
            }
        });
    }
}
