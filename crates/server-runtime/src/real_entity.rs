//! Real entities on a CellApp and their tick faces
//! (`server/cellapp/real_entity.*`, `entity.cpp`, `entity_callback_buffer.cpp`,
//! `cell.cpp`'s `Cell::Entities`).
//!
//! A real entity is the entity one CellApp serves from its own cell: the
//! `Entity` half keeps identity, liveness and the script object; the
//! `RealEntity` half keeps the location state — velocity, position samples,
//! the controlled-by mailbox, witnesses, haunts, the auto-backup policy —
//! and the removal handle into `Cell::Entities` (the backup rotation's
//! list).
//!
//! The acceptance for BW-08-02-01 is the script callback order: `Cell::
//! createEntityInternal` (`cell.cpp:349-437`) wraps the creation in
//! `Entity::callbacksPermitted(false)`/`(true)`, and `Entity::initReal`
//! (`entity.cpp:1425-1531`) brackets `callScriptInit` with high-priority
//! buffering — so `__init__` (or `onRestore`) fires from the buffer flush,
//! high-priority callbacks first, and any callback queued by the init
//! itself runs nested inside it, in order. The port records the executed
//! sequence in [`EntityCallbackBuffer`].
//!
//! The per-tick face is the backup rotation: `CellApp::tickBackup`
//! (`cellapp.cpp:1325-1333`) → `Cell::backup(index, period)`
//! (`cell.cpp:448-474`) walks the real entities in removal-handle order and
//! auto-backs up the slice `size * (index+1) / period`.

use std::collections::BTreeMap;

use foundation::math::Vector3;

use crate::cellappmgr::{Addr, SpaceId};
use crate::entity_dispatch::EntityId;
use crate::space::Space;

/// `AutoBackupAndArchive::Policy` (`lib/server/auto_backup_and_archive.hpp`).
/// The default a fresh `RealEntity` carries.
pub const AUTO_BACKUP_YES: u8 = 1;

/// `NO_ENTITY_REMOVAL_HANDLE` (`cell.hpp:35`): not in the collection.
pub const NO_ENTITY_REMOVAL_HANDLE: usize = usize::MAX;

// -----------------------------------------------------------------------------
// Section: The real entity record (`real_entity.hpp`)
// -----------------------------------------------------------------------------

/// `EntityMailBoxRef` (`entity.hpp`): the base mailbox an entity is
/// controlled by; empty until the script sets `controlledBy`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EntityMailBoxRef {
    /// `EntityMailBoxRef::addr_`.
    pub addr: Addr,
    /// `EntityMailBoxRef::id`.
    pub id: EntityId,
}

impl EntityMailBoxRef {
    /// An empty reference (`EntityMailBoxRef::init`).
    pub fn empty() -> Self {
        Self {
            addr: (0, 0),
            id: 0,
        }
    }
}

/// One `RealEntity::Haunt` (`real_entity.hpp:120-135`): a channel from this
/// real entity's witness to a watching client.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Haunt {
    /// `Haunt::addr_`.
    pub addr: Addr,
    /// `Haunt::creationTime_`.
    pub creation_time: u32,
}

/// The real side of an entity (`real_entity.hpp:74-206`): the location
/// state `Entity::createReal` constructs and `RealEntity::init` seeds.
#[derive(Clone, Debug, PartialEq)]
pub struct RealEntity {
    /// `Entity::id()`.
    pub id: EntityId,
    /// The base address the entity's channel talks to.
    pub base_addr: Addr,
    /// `creationTime_` — the app time the entity was created at.
    pub creation_time: u32,
    /// `velocity_`.
    pub velocity: Vector3,
    /// `positionSample_`.
    pub position_sample: Vector3,
    /// `positionSampleTime_`.
    pub position_sample_time: u32,
    /// `shouldAutoBackup_` — the `AutoBackupAndArchive::Policy`.
    pub should_auto_backup: u8,
    /// `controlledBy_` — empty until set.
    pub controlled_by: EntityMailBoxRef,
    /// `hasWitness` — `pWitness_ != NULL`.
    pub has_witness: bool,
    /// `haunts_`.
    pub haunts: Vec<Haunt>,
    /// The removal handle into `Cell::Entities` (`NO_ENTITY_REMOVAL_HANDLE`
    /// until added).
    pub removal_handle: usize,
    /// `Entity::startRealControllers` ran for a restore (`initReal`,
    /// `entity.cpp:1484-1487`) — the controllers are BW-08-02-02's face.
    pub controllers_started: bool,
}

impl RealEntity {
    /// `RealEntity::RealEntity` (`real_entity.cpp:115-140`): the position
    /// sample starts at the creation position, sampled at the app time.
    fn new(
        id: EntityId,
        base_addr: Addr,
        position: Vector3,
        now: u32,
        should_auto_backup: u8,
    ) -> Self {
        Self {
            id,
            base_addr,
            creation_time: now,
            velocity: Vector3::ZERO,
            position_sample: position,
            position_sample_time: now,
            should_auto_backup,
            controlled_by: EntityMailBoxRef::empty(),
            has_witness: false,
            haunts: Vec::new(),
            removal_handle: NO_ENTITY_REMOVAL_HANDLE,
            controllers_started: false,
        }
    }

    /// `RealEntity::autoBackup` (`real_entity.cpp:878-891`): a `NO` policy
    /// skips the backup entirely, `NEXT_ONLY` backs up once and drops to
    /// `NO`.
    pub fn auto_backup(&mut self, out: &mut Vec<CellEntityOutbound>) {
        if self.should_auto_backup == AUTO_BACKUP_NO {
            return;
        }
        self.backup(out);
        if self.should_auto_backup == AUTO_BACKUP_NEXT_ONLY {
            self.should_auto_backup = AUTO_BACKUP_NO;
        }
    }

    /// `RealEntity::backup` (`real_entity.cpp:898-920`): the
    /// `backupCellEntity` bundle for an entity with a base.
    pub fn backup(&self, out: &mut Vec<CellEntityOutbound>) {
        out.push(CellEntityOutbound::BackupEntity { entity_id: self.id });
    }
}

/// `AutoBackupAndArchive::Policy::NO`.
pub const AUTO_BACKUP_NO: u8 = 0;
/// `AutoBackupAndArchive::Policy::NEXT_ONLY`.
pub const AUTO_BACKUP_NEXT_ONLY: u8 = 2;

// -----------------------------------------------------------------------------
// Section: The callback buffer (`entity_callback_buffer.*`)
// -----------------------------------------------------------------------------

/// Where `Entity::callback` sent a script callback
/// (`entity.cpp:7922-7990`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CallbackDisposition {
    /// Ran immediately — callbacks were permitted.
    Executed,
    /// Queued for a later flush — callbacks were prevented.
    Stored,
    /// Refused — the entity was destroyed.
    DestroyedRefused,
}

/// The recorded callback (`BufferedScriptCallback`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BufferedScriptCallback {
    /// `bsc.entity->id()`.
    pub entity_id: EntityId,
    /// `bsc.callable` — the method name.
    pub method: String,
}

/// `EntityCallbackBuffer` (`entity_callback_buffer.cpp`): callbacks
/// arriving while they are prevented queue; the buffered section's end
/// flushes, high-priority queue first, then the ordinary one. A callback
/// arriving while none is prevented runs immediately — the `Entity::
/// callback` direct path.
pub struct EntityCallbackBuffer {
    /// `callbacksPrevented_`.
    callbacks_prevented: i32,
    /// `allowCallbacksOverride_`.
    allow_callbacks_override: i32,
    /// `buffer_` — the ordinary queue.
    buffer: Vec<BufferedScriptCallback>,
    /// `highPriorityBuffer_`.
    high_priority_buffer: Vec<BufferedScriptCallback>,
    /// The callbacks executed, in order — the accepted record.
    executed: Vec<BufferedScriptCallback>,
    /// The destroyed entities skipped, with a warning each
    /// (`execute`'s `WARNING_MSG`).
    skipped_destroyed: Vec<BufferedScriptCallback>,
}

impl Default for EntityCallbackBuffer {
    fn default() -> Self {
        Self::new()
    }
}

impl EntityCallbackBuffer {
    /// A fresh, empty buffer.
    pub fn new() -> Self {
        Self {
            callbacks_prevented: 0,
            allow_callbacks_override: 0,
            buffer: Vec::new(),
            high_priority_buffer: Vec::new(),
            executed: Vec::new(),
            skipped_destroyed: Vec::new(),
        }
    }

    /// `Entity::callbacksPermitted()` (`entity.cpp:680`): callbacks are
    /// permitted exactly when nothing is prevented — the reference's
    /// `isBuffering()` naming is inverted from its callers'.
    pub fn callbacks_permitted(&self) -> bool {
        self.callbacks_prevented == 0
    }

    /// `enableHighPriorityBuffering` / `disableHighPriorityBuffering`.
    pub fn enable_high_priority_buffering(&mut self) {
        self.allow_callbacks_override += 1;
    }

    pub fn disable_high_priority_buffering(&mut self) {
        self.allow_callbacks_override -= 1;
    }

    /// `enterBufferedSection` — callbacks now queue.
    pub fn enter_buffered_section(&mut self) {
        self.callbacks_prevented += 1;
    }

    /// `leaveBufferedSection` — the section's end flushes when the count
    /// reaches zero, consulting the owner's liveness lookup.
    pub fn leave_buffered_section(&mut self, is_destroyed: &dyn Fn(EntityId) -> bool) {
        self.callbacks_prevented -= 1;
        if self.callbacks_prevented == 0 {
            self.flush(is_destroyed);
        }
    }

    /// `storeCallback`'s routing (`entity_callback_buffer.cpp:113-126`):
    /// high priority when the override covers the prevention depth.
    pub fn store_callback(&mut self, entity_id: EntityId, method: &str) {
        let callback = BufferedScriptCallback {
            entity_id,
            method: method.to_owned(),
        };
        let high_priority = self.allow_callbacks_override >= self.callbacks_prevented;
        if high_priority {
            self.high_priority_buffer.push(callback);
        } else {
            self.buffer.push(callback);
        }
    }

    /// `flush` — swap both queues out and execute the high-priority one
    /// first, then the ordinary one; reentrant arrivals queue for the next
    /// flush (the reference's `execute` note).
    pub fn flush(&mut self, is_destroyed: &dyn Fn(EntityId) -> bool) {
        let high = std::mem::take(&mut self.high_priority_buffer);
        let normal = std::mem::take(&mut self.buffer);
        self.execute(high, is_destroyed);
        self.execute(normal, is_destroyed);
    }

    /// `execute` — destroyed entities warn and skip; live ones run in
    /// order.
    fn execute(
        &mut self,
        script_calls: Vec<BufferedScriptCallback>,
        is_destroyed: &dyn Fn(EntityId) -> bool,
    ) {
        for callback in script_calls {
            if is_destroyed(callback.entity_id) {
                self.skipped_destroyed.push(callback);
            } else {
                self.executed.push(callback);
            }
        }
    }

    /// `Entity::callback` (`entity.cpp:7922-7990`): destroyed refuses,
    /// prevented queues, permitted runs now.
    pub fn callback(
        &mut self,
        entity_id: EntityId,
        method: &str,
        destroyed: bool,
    ) -> CallbackDisposition {
        if destroyed {
            return CallbackDisposition::DestroyedRefused;
        }
        if !self.callbacks_permitted() {
            self.store_callback(entity_id, method);
            CallbackDisposition::Stored
        } else {
            self.executed.push(BufferedScriptCallback {
                entity_id,
                method: method.to_owned(),
            });
            CallbackDisposition::Executed
        }
    }

    /// The executed callbacks, in execution order.
    pub fn executed(&self) -> &[BufferedScriptCallback] {
        &self.executed
    }

    /// The destroyed entities a flush skipped, with their warnings.
    pub fn skipped_destroyed(&self) -> &[BufferedScriptCallback] {
        &self.skipped_destroyed
    }
}

// -----------------------------------------------------------------------------
// Section: The entity population (`Entity::population()`)
// -----------------------------------------------------------------------------

/// One population record (`EntityPopulation`: real/ghost/zombie, the
/// address the ghost's real lives at).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PopulationRecord {
    /// The entity is real to this app.
    pub is_real: bool,
    /// The real's address, for a ghost record.
    pub real_addr: Option<Addr>,
    /// `Entity::isDestroyed()`.
    pub destroyed: bool,
}

/// `EntityPopulation` — the id-indexed entity liveness index
/// (`entity.cpp`'s `population_`), the dup guard and the callback buffer's
/// liveness lookup.
pub struct EntityPopulation {
    records: BTreeMap<EntityId, PopulationRecord>,
}

impl Default for EntityPopulation {
    fn default() -> Self {
        Self::new()
    }
}

impl EntityPopulation {
    /// A fresh, empty population.
    pub fn new() -> Self {
        Self {
            records: BTreeMap::new(),
        }
    }

    /// The population lookup — `execute`'s liveness test.
    pub fn is_destroyed(&self, id: EntityId) -> bool {
        self.records.get(&id).is_some_and(|record| record.destroyed)
    }

    /// A non-destroyed existing record, `EntityPopulation::find`'s guard.
    pub fn find_alive(&self, id: EntityId) -> Option<PopulationRecord> {
        self.records.get(&id).copied().filter(|r| !r.destroyed)
    }

    /// Record a fresh entity (real or ghost).
    pub fn insert(&mut self, id: EntityId, is_real: bool, real_addr: Option<Addr>) {
        self.records.insert(
            id,
            PopulationRecord {
                is_real,
                real_addr,
                destroyed: false,
            },
        );
    }

    /// Mark an entity destroyed (`Entity::destroy`'s slot update).
    pub fn mark_destroyed(&mut self, id: EntityId) {
        if let Some(record) = self.records.get_mut(&id) {
            record.destroyed = true;
        }
    }

    /// The live count.
    pub fn count(&self) -> usize {
        self.records.values().filter(|r| !r.destroyed).count()
    }
}

// -----------------------------------------------------------------------------
// Section: The cell's real entities (`Cell::Entities`, `cell.hpp:51-83`)
// -----------------------------------------------------------------------------

/// One `Cell::Entities::add` randomization pick: after every add the
/// reference swaps a random existing element to the back so the backup
/// rotation is randomized (`cell.cpp:916-927`). The port threads the pick
/// so the order stays testable.
fn swap_pick(rng: u64, size: usize) -> usize {
    (rng as usize) % size
}

/// The `Cell::Entities` collection: real entities in removal-handle
/// order, with the swap-with-back add and removal.
#[derive(Default)]
pub struct CellEntityCollection {
    reals: Vec<EntityId>,
    handles: BTreeMap<EntityId, usize>,
}

impl CellEntityCollection {
    /// `Cell::Entities::size`.
    pub fn size(&self) -> usize {
        self.reals.len()
    }

    /// `Cell::Entities::front`.
    pub fn front(&self) -> Option<EntityId> {
        self.reals.first().copied()
    }

    /// The ids, in removal-handle order.
    pub fn ids(&self) -> &[EntityId] {
        &self.reals
    }

    /// An entity's removal handle.
    pub fn removal_handle(&self, id: EntityId) -> Option<usize> {
        self.handles.get(&id).copied()
    }

    /// `Cell::Entities::add` (`cell.cpp:900-929`): the handle is the
    /// collection size, then a (threaded) existing element swaps to the
    /// back.
    pub fn add(&mut self, id: EntityId, rng: u64) {
        let handle = self.reals.len();
        self.reals.push(id);
        self.handles.insert(id, handle);
        let pick = swap_pick(rng, self.reals.len());
        self.swap_with_back(pick);
    }

    /// `Cell::swapWithBack` (`cell.cpp:937-947`).
    fn swap_with_back(&mut self, handle: usize) {
        let back = self.reals.len() - 1;
        if handle == back {
            return;
        }
        let picked = self.reals[handle];
        let back_id = self.reals[back];
        self.reals[back] = picked;
        self.reals[handle] = back_id;
        *self.handles.get_mut(&picked).unwrap() = back;
        *self.handles.get_mut(&back_id).unwrap() = handle;
    }

    /// `Cell::Entities::remove` (`cell.cpp:953-990`): the back fills the
    /// hole, so the handles stay dense.
    pub fn remove(&mut self, id: EntityId) -> bool {
        let Some(&handle) = self.handles.get(&id) else {
            return false;
        };
        let back = self.reals.len() - 1;
        if handle == back {
            self.reals.pop();
        } else {
            let back_id = self.reals[back];
            self.reals[handle] = back_id;
            *self.handles.get_mut(&back_id).unwrap() = handle;
            self.reals.pop();
        }
        self.handles.remove(&id);
        true
    }
}

// -----------------------------------------------------------------------------
// Section: The creation flow (`Cell::createEntityInternal` → `initReal`)
// -----------------------------------------------------------------------------

/// The outbound messages a creation sends
/// (`BaseAppIntInterface::currentCell`, `backupCellEntity`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CellEntityOutbound {
    /// `Entity::informBaseOfAddress` (`entity.cpp:3212-3260`): the
    /// reliable-critical current-cell message teaching the base where the
    /// cell part now lives.
    CurrentCell {
        /// The entity id.
        entity_id: EntityId,
        /// `currentCellArgs.newSpaceID`.
        new_space_id: SpaceId,
        /// `currentCellArgs.newCellAddr`.
        new_cell_addr: Addr,
    },
    /// A `backupCellEntity` bundle.
    BackupEntity {
        /// The entity id.
        entity_id: EntityId,
    },
}

/// `Entity::callScriptInit`'s choice (`entity.cpp:1916-1938`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScriptInitKind {
    /// `onRestore` — a restore from the base's backup.
    OnRestore,
    /// `__init__` with the nearby entity as its argument.
    InitWithNearby(Option<EntityId>),
    /// Plain `__init__`.
    Init,
}

/// What a creation needs (`Cell::createEntityInternal`'s input).
#[derive(Clone, Debug, PartialEq)]
pub struct CreateRealRequest {
    /// `data >> id >> entityTypeID`: zero means "allocate from the ID
    /// client" (`id_alloc` supplies it).
    pub id: EntityId,
    /// The entity type id.
    pub type_id: u16,
    /// `readRealDataInEntityFromStreamForInitOrRestore`'s fields.
    pub position: Vector3,
    /// `isOnGroundFlag`.
    pub is_on_ground: bool,
    /// `(yaw, pitch, roll)`.
    pub direction: (f32, f32, f32),
    /// `lastEventNumber_`.
    pub last_event_number: u32,
    /// `createRealInfo == CREATE_REAL_FROM_RESTORE`.
    pub is_restore: bool,
    /// `pEntityType_->expectsNearbyEntity()`.
    pub expects_nearby_entity: bool,
    /// The nearby entity for `__init__`.
    pub nearby: Option<EntityId>,
    /// `RealEntity::init`'s `shouldAutoBackup_`.
    pub should_auto_backup: u8,
    /// `RealEntity::init`'s `controlledBy_`.
    pub controlled_by: EntityMailBoxRef,
    /// Models the script destroying the entity inside `__init__`
    /// (`Cell::createEntityInternal`'s TODO at `cell.cpp:408-411`).
    pub destroy_in_init: bool,
}

impl Default for CreateRealRequest {
    fn default() -> Self {
        Self {
            id: 0,
            type_id: 0,
            position: Vector3::ZERO,
            is_on_ground: false,
            direction: (0.0, 0.0, 0.0),
            last_event_number: 0,
            is_restore: false,
            expects_nearby_entity: false,
            nearby: None,
            should_auto_backup: AUTO_BACKUP_YES,
            controlled_by: EntityMailBoxRef::empty(),
            destroy_in_init: false,
        }
    }
}

/// The creation outcome (`Cell::createEntityInternal`'s return faces).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CreateEntityOutcome {
    /// The entity was created; `zombie_destroyed` records the zombie
    /// ghost a restore destroyed before re-creating.
    Created {
        /// The entity id (allocated when the request asked for zero).
        id: EntityId,
        /// Which init callback ran.
        script_init: ScriptInitKind,
        /// The script destroyed the entity inside its init callback.
        destroyed_in_init: bool,
        /// A zombie ghost was destroyed prior to the restore.
        zombie_destroyed: bool,
    },
    /// An already-existing live entity — the dup guard
    /// (`cell.cpp:382-398`).
    AlreadyExists(EntityId),
    /// The ID client had no free ids (`cell.cpp:359-365`).
    NoFreeIds,
    /// `Space::addEntity` refused the slot (`cell.cpp:403`).
    SpaceRefused(EntityId),
}

/// One cell's real entities, its population records and the shared
/// callback buffer — the `Cell` half of `createEntityInternal`.
pub struct CellEntities {
    /// The real entity records.
    reals: BTreeMap<EntityId, RealEntity>,
    /// `realEntities_` — the backup rotation's list.
    collection: CellEntityCollection,
    /// `Entity::population()`.
    population: EntityPopulation,
    /// `s_callbackBuffer_` — the app's one buffer; the cell borrows it
    /// through the owning `CellApp` face.
    callbacks: EntityCallbackBuffer,
    /// `Cell::backupIndex_`.
    backup_index: i32,
}

impl Default for CellEntities {
    fn default() -> Self {
        Self::new()
    }
}

impl CellEntities {
    /// A fresh, empty cell.
    pub fn new() -> Self {
        Self {
            reals: BTreeMap::new(),
            collection: CellEntityCollection::default(),
            population: EntityPopulation::new(),
            callbacks: EntityCallbackBuffer::new(),
            backup_index: 0,
        }
    }

    /// The executed script callbacks, in execution order.
    pub fn executed(&self) -> &[BufferedScriptCallback] {
        self.callbacks.executed()
    }

    /// The destroyed entities a flush skipped.
    pub fn skipped_destroyed(&self) -> &[BufferedScriptCallback] {
        self.callbacks.skipped_destroyed()
    }

    /// The real entity records.
    pub fn reals(&self) -> &BTreeMap<EntityId, RealEntity> {
        &self.reals
    }

    /// A real entity record.
    pub fn real(&self, id: EntityId) -> Option<&RealEntity> {
        self.reals.get(&id)
    }

    /// The population records — the dup guard and the liveness lookup.
    pub fn population(&mut self) -> &mut EntityPopulation {
        &mut self.population
    }

    /// The real ids in removal-handle order.
    pub fn real_ids(&self) -> &[EntityId] {
        self.collection.ids()
    }

    /// A real entity record, mutable — the auto-backup policy face.
    pub fn real_mut(&mut self, id: EntityId) -> Option<&mut RealEntity> {
        self.reals.get_mut(&id)
    }

    /// The collection size (`Cell::Entities::size`).
    pub fn collection_size(&self) -> usize {
        self.collection.size()
    }

    /// `Entity::informBaseOfAddress` + `Cell::Entities::add` — the
    /// `addRealEntity` face (`cell.cpp:237-248`).
    fn add_real_entity(
        &mut self,
        id: EntityId,
        space_id: SpaceId,
        app_addr: Addr,
        now: u32,
        rng: u64,
        out: &mut Vec<CellEntityOutbound>,
    ) {
        let real = self
            .reals
            .get_mut(&id)
            .expect("addRealEntity on an entity with no RealEntity");
        out.push(CellEntityOutbound::CurrentCell {
            entity_id: id,
            new_space_id: space_id,
            new_cell_addr: app_addr,
        });
        real.creation_time = now;
        self.collection.add(id, rng);
    }

    /// `Cell::createEntityInternal` (`cell.cpp:349-437`) and
    /// `Entity::initReal` (`entity.cpp:1425-1531`): the dup guard, the
    /// population record, the buffered init callback and the immediate
    /// backup.
    #[allow(clippy::too_many_arguments)]
    pub fn create_entity(
        &mut self,
        space: &mut Space,
        request: CreateRealRequest,
        app_addr: Addr,
        id_alloc: &mut dyn FnMut() -> Option<EntityId>,
        now: u32,
        rng: u64,
        out: &mut Vec<CellEntityOutbound>,
    ) -> CreateEntityOutcome {
        // `cell.cpp:353-380` — the id, and the dup guard.
        let id = if request.id != 0 {
            request.id
        } else {
            match id_alloc() {
                Some(allocated) => allocated,
                None => return CreateEntityOutcome::NoFreeIds,
            }
        };
        // `cell.cpp:382-398` — an existing live entity is the dup guard;
        // a restore over a zombie ghost destroys the ghost first (the
        // reference's destroyZombie loop; without buffered ghost messages
        // it is one pass).
        let mut zombie_destroyed = false;
        if let Some(record) = self.population.find_alive(id) {
            if request.is_restore && !record.is_real {
                self.population.mark_destroyed(id);
                zombie_destroyed = true;
            } else {
                return CreateEntityOutcome::AlreadyExists(id);
            }
        }

        // `Space::newEntity` — the space's entity slot.
        if space.add_entity(id, true).is_none() {
            return CreateEntityOutcome::SpaceRefused(id);
        }
        self.population.insert(id, true, Some(app_addr));

        // `cell.cpp:390-394` — the callbacks-permitted section wraps the
        // whole creation.
        self.callbacks.enter_buffered_section();

        // `Entity::initReal` — read the data, create the RealEntity,
        // seed its controllable state.
        let real = RealEntity::new(
            id,
            request.controlled_by.addr,
            request.position,
            now,
            request.should_auto_backup,
        );
        self.reals.insert(id, real);

        // `entity.cpp:1484-1487` — restore starts real controllers
        // (BW-08-02-02's face).
        if request.is_restore {
            self.reals.get_mut(&id).unwrap().controllers_started = true;
        }

        // `cell.cpp:403` — add to the real list; the current-cell message
        // rides the same bundle as the first backup.
        self.add_real_entity(id, space.id(), app_addr, now, rng, out);

        // `entity.cpp:1503-1508` — high-priority buffering around the
        // init callback.
        self.callbacks.enable_high_priority_buffering();
        let script_init = if request.is_restore {
            ScriptInitKind::OnRestore
        } else if request.expects_nearby_entity {
            ScriptInitKind::InitWithNearby(request.nearby)
        } else {
            ScriptInitKind::Init
        };
        let init_method = match script_init {
            ScriptInitKind::OnRestore => "onRestore",
            ScriptInitKind::InitWithNearby(_) | ScriptInitKind::Init => "__init__",
        };
        self.callbacks
            .callback(id, init_method, self.population.is_destroyed(id));
        self.callbacks.disable_high_priority_buffering();

        // `cell.cpp:396` — the section's end flushes the buffer.
        self.callbacks
            .leave_buffered_section(&|entity_id| self.population.is_destroyed(entity_id));

        // The script may have destroyed the entity in its init callback —
        // `createEntityInternal`'s TODO: skip the backup.
        if request.destroy_in_init {
            self.destroy(id);
            return CreateEntityOutcome::Created {
                id,
                script_init,
                destroyed_in_init: true,
                zombie_destroyed,
            };
        }

        // `cell.cpp:433` — the backup goes out on the same bundle; the
        // destroy-in-init case returned above, so the real is present.
        self.reals
            .get(&id)
            .expect("a surviving creation has its real")
            .backup(out);

        CreateEntityOutcome::Created {
            id,
            script_init,
            destroyed_in_init: false,
            zombie_destroyed,
        }
    }

    /// `Cell::backup(index, period)` (`cell.cpp:448-474`) — the per-tick
    /// rotation: the slice `size * (index+1) / period` auto-backs-up.
    pub fn backup(&mut self, index: i32, period: i32, out: &mut Vec<CellEntityOutbound>) {
        if period == 0 {
            return;
        }
        let new_backup_index = (self.collection.size() as i32) * (index + 1) / period;
        if index == 0 {
            self.backup_index = 0;
        }
        if self.backup_index < new_backup_index {
            let start = self.backup_index as usize;
            let end = new_backup_index as usize;
            let ids: Vec<EntityId> = self.collection.ids()[start..end].to_vec();
            for id in ids {
                let real = self.reals.get_mut(&id).expect("the rotation walks reals");
                real.auto_backup(out);
            }
        }
        self.backup_index = new_backup_index;
    }

    /// `Cell::entityDestroyed` (`cell.cpp:260-289`) — the entity's `die`
    /// has run; remove it from the real list and mark the population.
    pub fn destroy(&mut self, id: EntityId) -> bool {
        if self.reals.remove(&id).is_none() {
            return false;
        }
        self.collection.remove(id);
        self.population.mark_destroyed(id);
        true
    }
}

// -----------------------------------------------------------------------------
// Section: TDD — BW-08-02-01 创建 Real 实体并推进 Tick
// -----------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::space::{Space, SpaceNode, SpaceOutbound};

    fn request(id: EntityId) -> CreateRealRequest {
        CreateRealRequest {
            id,
            ..CreateRealRequest::default()
        }
    }

    /// The acceptance: a fresh creation runs exactly `__init__` from the
    /// callback flush, the base learns the cell address and the backup
    /// goes out immediately.
    #[test]
    fn creating_a_real_entity_runs_the_reference_order() {
        let mut space = Space::new(7, false);
        let mut cell = CellEntities::new();
        let mut out = Vec::new();
        let mut alloc = || Some(42);

        let outcome = cell.create_entity(
            &mut space,
            request(0),
            (10, 1),
            &mut alloc,
            100,
            7,
            &mut out,
        );
        assert_eq!(
            outcome,
            CreateEntityOutcome::Created {
                id: 42,
                script_init: ScriptInitKind::Init,
                destroyed_in_init: false,
                zombie_destroyed: false,
            }
        );

        // `__init__` ran from the flush, in order.
        assert_eq!(
            cell.executed(),
            &[BufferedScriptCallback {
                entity_id: 42,
                method: "__init__".to_owned(),
            }]
        );

        // The current-cell message, then the immediate backup.
        assert_eq!(
            out,
            vec![
                CellEntityOutbound::CurrentCell {
                    entity_id: 42,
                    new_space_id: 7,
                    new_cell_addr: (10, 1),
                },
                CellEntityOutbound::BackupEntity { entity_id: 42 },
            ]
        );

        // The population and the real record.
        assert_eq!(cell.population().count(), 1);
        let real = cell.real(42).unwrap();
        assert_eq!(real.id, 42);
        assert_eq!(real.creation_time, 100);
        assert_eq!(real.position_sample, Vector3::ZERO);
        assert_eq!(real.should_auto_backup, AUTO_BACKUP_YES);
        assert_eq!(real.controlled_by, EntityMailBoxRef::empty());
    }

    /// `callScriptInit`: a restore runs `onRestore`; a type expecting a
    /// nearby entity passes it to `__init__`.
    #[test]
    fn the_init_callback_kind_follows_the_reference() {
        let mut space = Space::new(1, false);
        let mut cell = CellEntities::new();
        let mut out = Vec::new();
        let mut next_id = 1;
        let mut alloc = move || {
            let id = next_id;
            next_id += 1;
            Some(id)
        };

        let restore = cell.create_entity(
            &mut space,
            CreateRealRequest {
                id: 0,
                is_restore: true,
                ..CreateRealRequest::default()
            },
            (10, 1),
            &mut alloc,
            200,
            1,
            &mut out,
        );
        assert!(matches!(
            restore,
            CreateEntityOutcome::Created {
                script_init: ScriptInitKind::OnRestore,
                ..
            }
        ));

        let nearby = cell.create_entity(
            &mut space,
            CreateRealRequest {
                id: 0,
                expects_nearby_entity: true,
                nearby: Some(99),
                ..CreateRealRequest::default()
            },
            (10, 1),
            &mut alloc,
            300,
            1,
            &mut out,
        );
        assert!(matches!(
            nearby,
            CreateEntityOutcome::Created {
                script_init: ScriptInitKind::InitWithNearby(Some(99)),
                ..
            }
        ));

        assert_eq!(
            cell.executed()
                .iter()
                .map(|c| c.method.as_str())
                .collect::<Vec<_>>(),
            vec!["onRestore", "__init__"]
        );
    }

    /// A high-priority callback queued by the init callback runs nested
    /// inside it, before the ordinary queue's callbacks.
    #[test]
    fn high_priority_callbacks_run_nested_inside_the_init() {
        let mut buffer = EntityCallbackBuffer::new();
        buffer.enter_buffered_section();
        buffer.enable_high_priority_buffering();
        buffer.callback(5, "__init__", false);
        buffer.disable_high_priority_buffering();
        // Queued while the init runs: high-priority buffering is off, the
        // section is still open — ordinary queue.
        buffer.callback(5, "queued_by_init", false);
        buffer.leave_buffered_section(&|_| false);
        assert_eq!(
            buffer
                .executed()
                .iter()
                .map(|c| c.method.as_str())
                .collect::<Vec<_>>(),
            vec!["__init__", "queued_by_init"]
        );
    }

    /// A callback arriving while nothing is prevented runs immediately.
    #[test]
    fn an_unprevented_callback_runs_immediately() {
        let mut buffer = EntityCallbackBuffer::new();
        assert_eq!(
            buffer.callback(5, "onWitnessed", false),
            CallbackDisposition::Executed
        );
        assert_eq!(buffer.executed().len(), 1, "no buffering happened");
    }

    /// Destroyed entities are skipped with a warning at flush time.
    #[test]
    fn destroyed_entities_are_skipped_with_a_warning() {
        let mut buffer = EntityCallbackBuffer::new();
        buffer.enter_buffered_section();
        buffer.callback(5, "__init__", false);
        buffer.callback(6, "onWitnessed", false);
        buffer.leave_buffered_section(&|id| id == 6);
        assert_eq!(
            buffer
                .executed()
                .iter()
                .map(|c| c.entity_id)
                .collect::<Vec<_>>(),
            vec![5]
        );
        assert_eq!(
            buffer
                .skipped_destroyed()
                .iter()
                .map(|c| c.entity_id)
                .collect::<Vec<_>>(),
            vec![6]
        );
    }

    /// The dup guard and the ID client's exhaustion.
    #[test]
    fn an_existing_entity_is_refused_and_no_free_ids_refuses_the_creation() {
        let mut space = Space::new(3, false);
        let mut cell = CellEntities::new();
        let mut out = Vec::new();
        assert!(matches!(
            cell.create_entity(
                &mut space,
                request(42),
                (10, 1),
                &mut || Some(42),
                1,
                0,
                &mut out
            ),
            CreateEntityOutcome::Created { .. }
        ));
        assert_eq!(
            cell.create_entity(
                &mut space,
                request(42),
                (10, 1),
                &mut || Some(42),
                2,
                0,
                &mut out
            ),
            CreateEntityOutcome::AlreadyExists(42)
        );

        let mut exhausted = || None;
        assert_eq!(
            cell.create_entity(
                &mut space,
                request(0),
                (10, 1),
                &mut exhausted,
                3,
                0,
                &mut out
            ),
            CreateEntityOutcome::NoFreeIds
        );
    }

    /// A restore over a zombie ghost destroys the ghost first.
    #[test]
    fn a_restore_over_a_zombie_destroys_the_ghost_first() {
        let mut space = Space::new(5, false);
        let mut cell = CellEntities::new();
        let mut out = Vec::new();
        let mut alloc = || Some(7);

        // The ghost half first (as a fault window would leave it).
        cell.population().insert(7, false, Some((10, 1)));
        let outcome = cell.create_entity(
            &mut space,
            CreateRealRequest {
                id: 0,
                is_restore: true,
                ..CreateRealRequest::default()
            },
            (10, 1),
            &mut alloc,
            1,
            1,
            &mut out,
        );
        assert!(matches!(
            outcome,
            CreateEntityOutcome::Created {
                zombie_destroyed: true,
                ..
            }
        ));
    }

    /// The script destroying the entity in `__init__` skips the backup
    /// and cleans the real list.
    #[test]
    fn a_destroy_inside_init_skips_the_backup() {
        let mut space = Space::new(9, false);
        let mut cell = CellEntities::new();
        let mut out = Vec::new();
        let mut alloc = || Some(11);
        let outcome = cell.create_entity(
            &mut space,
            CreateRealRequest {
                id: 0,
                destroy_in_init: true,
                ..CreateRealRequest::default()
            },
            (10, 1),
            &mut alloc,
            1,
            1,
            &mut out,
        );
        assert!(matches!(
            outcome,
            CreateEntityOutcome::Created {
                destroyed_in_init: true,
                ..
            }
        ));
        assert!(
            !out.contains(&CellEntityOutbound::BackupEntity { entity_id: 11 }),
            "no backup for an entity the init destroyed"
        );
        assert!(cell.reals().is_empty());
        assert!(cell.real_ids().is_empty());
    }

    /// `Cell::backup(index, period)` walks the real list in handle order
    /// and auto-back-up skips the `NO` policy.
    #[test]
    fn the_backup_rotation_walks_the_real_list_in_handle_order() {
        let mut space = Space::new(2, false);
        let mut cell = CellEntities::new();
        let mut out = Vec::new();
        let mut next = 1;
        let mut alloc = move || {
            let id = next;
            next += 1;
            Some(id)
        };
        for pick in 0..5u64 {
            cell.create_entity(
                &mut space,
                request(0),
                (10, 1),
                &mut alloc,
                1,
                pick,
                &mut out,
            );
        }
        let created: Vec<EntityId> = cell.real_ids().to_vec();
        assert_eq!(created.len(), 5);
        // The swap-with-back add randomizes; the handles stay dense.
        assert_eq!(cell.collection_size(), 5);
        out.clear();

        let mut backed = Vec::new();
        cell.backup(0, 3, &mut out);
        backed.push(
            out.iter()
                .filter(|m| matches!(m, CellEntityOutbound::BackupEntity { .. }))
                .count(),
        );
        out.clear();
        cell.backup(1, 3, &mut out);
        backed.push(out.len());
        out.clear();
        cell.backup(2, 3, &mut out);
        backed.push(out.len());
        // size=5, period=3: windows of 1, 2, 2 — five backups over the
        // rotation.
        assert_eq!(backed, vec![1, 2, 2]);

        // A `NO` policy entity is skipped.
        out.clear();
        let mut cell2 = CellEntities::new();
        let mut space2 = Space::new(21, false);
        for id in [1, 2] {
            cell2.create_entity(
                &mut space2,
                request(id),
                (10, 1),
                &mut alloc,
                1,
                id as u64,
                &mut out,
            );
        }
        out.clear();
        cell2.real_mut(1).unwrap().should_auto_backup = AUTO_BACKUP_NO;
        cell2.backup(0, 1, &mut out);
        assert_eq!(
            out,
            vec![CellEntityOutbound::BackupEntity { entity_id: 2 }],
            "the NO-policy entity is skipped, the other still backs up"
        );

        // A shrunken cell: the stored index sits past the new window, so
        // the pass backs nobody up (`cell.cpp:460`).
        let mut cell3 = CellEntities::new();
        let mut space3 = Space::new(31, false);
        let mut next3 = 1;
        let mut alloc3 = move || {
            let id = next3;
            next3 += 1;
            Some(id)
        };
        for pick in 0..5u64 {
            cell3.create_entity(
                &mut space3,
                request(0),
                (10, 1),
                &mut alloc3,
                1,
                pick,
                &mut out,
            );
        }
        out.clear();
        cell3.backup(2, 3, &mut out);
        for id in cell3.real_ids().to_vec() {
            cell3.destroy(id);
        }
        out.clear();
        cell3.backup(2, 3, &mut out);
        assert!(out.is_empty(), "the window sits behind the stored index");
    }

    /// `NEXT_ONLY` backs up once, then drops to `NO`.
    #[test]
    fn next_only_auto_backs_up_once() {
        let mut real = RealEntity::new(1, (10, 1), Vector3::ZERO, 5, AUTO_BACKUP_NEXT_ONLY);
        let mut out = Vec::new();
        real.auto_backup(&mut out);
        assert_eq!(out.len(), 1);
        real.auto_backup(&mut out);
        assert_eq!(out.len(), 1, "the policy dropped to NO");
        assert_eq!(real.should_auto_backup, AUTO_BACKUP_NO);
    }

    /// The swap-with-back add keeps the handles a dense permutation of
    /// the collection and the removal closes the hole with the back
    /// element.
    #[test]
    fn the_real_collection_keeps_dense_handles_across_add_and_remove() {
        let mut collection = CellEntityCollection::default();
        for id in [1, 2, 3, 4, 5] {
            collection.add(id, id as u64);
        }
        assert_eq!(collection.size(), 5);
        // Every handle names its entity exactly once — a permutation.
        let mut seen = [false; 5];
        for id in [1, 2, 3, 4, 5] {
            let handle = collection.removal_handle(id).unwrap();
            assert!(handle < 5);
            assert!(!seen[handle], "handle {handle} reused");
            seen[handle] = true;
            assert_eq!(collection.ids()[handle], id);
        }
        collection.remove(3);
        assert_eq!(collection.size(), 4);
        assert_eq!(collection.removal_handle(3), None);
        // The survivors still form a dense permutation.
        let mut seen = [false; 4];
        for id in [1, 2, 4, 5] {
            let handle = collection.removal_handle(id).unwrap();
            assert!(handle < 4);
            assert!(!seen[handle]);
            seen[handle] = true;
            assert_eq!(collection.ids()[handle], id);
        }
    }

    /// The population's liveness lookup drives the buffer.
    #[test]
    fn the_population_tracks_liveness() {
        let mut population = EntityPopulation::new();
        population.insert(3, true, Some((10, 1)));
        assert_eq!(population.count(), 1);
        assert!(!population.is_destroyed(3));
        population.mark_destroyed(3);
        assert!(population.is_destroyed(3));
        assert_eq!(population.count(), 0);
    }

    /// `Space::addEntity`'s guard is respected by the creation flow.
    #[test]
    fn the_space_slot_guard_refuses_a_duplicate() {
        let mut space = Space::new(4, false);
        assert!(space.add_entity(8, true).is_some());
        assert!(space.add_entity(8, true).is_none());
    }

    /// `Space::removeEntity`'s slot removal is the teardown face.
    #[test]
    fn removing_an_entity_frees_its_slot() {
        let mut space = Space::new(4, false);
        space.add_entity(8, true);
        let context = crate::space::CellAppContext::default();
        let mut out = crate::space::SpaceOutbound::default();
        assert!(space.remove_entity(8, &context, &mut out));
        assert!(!space.remove_entity(8, &context, &mut out));
    }

    /// A cell with a topology tree still serves its creations — the
    /// tree face coexists with the entity face.
    #[test]
    fn the_tree_and_the_entity_faces_coexist() {
        let mut space = Space::new(6, false);
        space.set_cell((10, 1));
        space.set_cell_infos(&[crate::space::CellInfoRecord::new(
            (10, 1),
            crate::cellappmgr::Rect::unbounded(),
        )]);
        let tree = SpaceNode::Leaf {
            addr: (10, 1),
            load: 0.0,
            has_been_created: true,
        };
        let mut out = SpaceOutbound::default();
        let report =
            space.update_geometry(&tree, &crate::space::CellAppContext::default(), &mut out);
        assert!(!report.was_multicell);

        let mut cell = CellEntities::new();
        let mut out = Vec::new();
        let mut alloc = || Some(13);
        let outcome =
            cell.create_entity(&mut space, request(0), (10, 1), &mut alloc, 1, 1, &mut out);
        assert!(matches!(
            outcome,
            CreateEntityOutcome::Created { id: 13, .. }
        ));
        assert_eq!(space.num_entities(), 1);
    }
    /// The remaining lifecycle edges: the three `Default`s, the
    /// collection's `front` and unknown-id removal, an unknown destroy,
    /// an unknown population mark, the period-zero backup and the empty
    /// skip log.
    #[test]
    fn the_remaining_edges_answer_false_and_default_empty() {
        let buffer = EntityCallbackBuffer::default();
        assert!(buffer.executed().is_empty());
        let population = EntityPopulation::default();
        assert_eq!(population.count(), 0);
        let cell = CellEntities::default();
        assert!(cell.executed().is_empty());
        assert!(cell.skipped_destroyed().is_empty());

        let mut collection = CellEntityCollection::default();
        assert_eq!(collection.front(), None);
        assert!(!collection.remove(9));
        collection.add(1, 0);
        assert_eq!(collection.front(), Some(1));

        let mut cell = CellEntities::new();
        assert!(!cell.destroy(99));

        let mut population = EntityPopulation::new();
        population.mark_destroyed(5);
        assert_eq!(population.count(), 0);

        let mut out = Vec::new();
        cell.backup(0, 0, &mut out);
        assert!(out.is_empty(), "period zero is no backup at all");
    }

    /// Nested buffered sections flush at the last close only, replaying
    /// everything in store order.
    #[test]
    fn nested_sections_flush_at_the_last_close() {
        let mut buffer = EntityCallbackBuffer::new();
        buffer.enter_buffered_section();
        buffer.callback(5, "outer", false);
        buffer.enter_buffered_section();
        buffer.callback(5, "inner", false);
        buffer.leave_buffered_section(&|_| false);
        assert!(
            buffer.executed().is_empty(),
            "the inner close does not flush"
        );
        buffer.callback(5, "after_inner", false);
        buffer.leave_buffered_section(&|_| false);
        assert_eq!(
            buffer
                .executed()
                .iter()
                .map(|c| c.method.as_str())
                .collect::<Vec<_>>(),
            vec!["outer", "inner", "after_inner"]
        );
    }

    /// `Entity::callback` on a destroyed entity refuses immediately.
    #[test]
    fn a_destroyed_entity_refuses_the_callback() {
        let mut buffer = EntityCallbackBuffer::new();
        assert_eq!(
            buffer.callback(5, "__init__", true),
            CallbackDisposition::DestroyedRefused
        );
        assert!(buffer.executed().is_empty());
    }

    /// A destroyed entity's slot keeps the space's guard: the re-create
    /// surfaces `SpaceRefused` (`Cell::createEntityInternal` passes the
    /// population guard after a destroy but the space slot is taken).
    #[test]
    fn the_space_slot_guard_surfaces_as_space_refused() {
        let mut space = Space::new(30, false);
        let mut cell = CellEntities::new();
        let mut out = Vec::new();
        assert!(matches!(
            cell.create_entity(
                &mut space,
                request(12),
                (10, 1),
                &mut || Some(12),
                1,
                0,
                &mut out
            ),
            CreateEntityOutcome::Created { id: 12, .. }
        ));
        assert!(cell.destroy(12));
        assert_eq!(
            cell.create_entity(
                &mut space,
                request(12),
                (10, 1),
                &mut || Some(12),
                2,
                0,
                &mut out
            ),
            CreateEntityOutcome::SpaceRefused(12)
        );
    }
}
