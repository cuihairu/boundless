//! The CellApp-side Space lifecycle (BW-08-01-01).
//!
//! Reference face: `server/cellapp/space.cpp` — `Space( SpaceID )`,
//! `addEntity`/`removeEntity` (the removal-handle swap-with-back, so an
//! `addEntity` that a removal trigger sets off stays correct),
//! `checkForShutDown`, `requestShutDown`, `shutDownSpace`'s one-shot arming
//! of the recurring `ShutdownSpace` timer, and `handleTimeout`'s
//! `onSpaceGone` + `destroyCell`.
//!
//! The manager half of the round trip already lives in
//! [`crate::cellappmgr::CellAppMgr::shut_down_space`]; this module is the
//! Space that sends the request when it empties and receives the message
//! back, so an empty Space is reclaimed: emptied, one timer-width later,
//! cell gone. The timer is the reference's recurring one-second timer —
//! armed once, cancelled only by the space's destruction — and it is
//! driven on an injected microsecond clock ([`Space::tick`]) instead of a
//! live dispatcher pass, which keeps the one-second delay observable.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;

use crate::cellappmgr::SpaceId;

/// `addTimer( 1000000, ... )` in `Space::shutDownSpace`.
pub const SHUTDOWN_SPACE_DELAY_US: u64 = 1_000_000;

/// The timer's name, as the reference registers it.
pub const SHUTDOWN_SPACE_TIMER: &str = "ShutdownSpace";

/// `NO_SPACE_REMOVAL_HANDLE`: an entity no space holds.
pub const NO_SPACE_REMOVAL_HANDLE: usize = usize::MAX;

/// `uint16(-1)`: the "delete this entry" key.
pub const NO_SPACE_DATA_KEY: u16 = u16::MAX;

/// `SPACE_DATA_TOD_KEY` (`common/space_data_types.hpp`): the time-of-day
/// payload, a `SpaceData_ToDData` of two floats.
pub const SPACE_DATA_TOD_KEY: u16 = 0;

/// `SPACE_DATA_MAPPING_KEY_CLIENT_SERVER`: the geometry-mapping entry —
/// the payload is a `SpaceData_MappingData` (a float matrix) followed by
/// the resource path. Mounting and unloading a physical space is driven
/// from this key.
pub const SPACE_DATA_MAPPING_KEY_CLIENT_SERVER: u16 = 1;

/// `SPACE_DATA_MAPPING_KEY_CLIENT_ONLY`: client-only geometry — the
/// reference's server branch is commented out and does nothing.
pub const SPACE_DATA_MAPPING_KEY_CLIENT_ONLY: u16 = 2;

/// `SPACE_DATA_FIRST_USER_KEY`.
pub const SPACE_DATA_FIRST_USER_KEY: u16 = 256;

/// `SPACE_DATA_FINAL_USER_KEY`.
pub const SPACE_DATA_FINAL_USER_KEY: u16 = 32767;

/// `SPACE_DATA_FIRST_CELL_ONLY_KEY`: keys from here up never reach the
/// clients, so they are kept out of `recentData_` and out of the
/// recently-deleted guard.
pub const SPACE_DATA_FIRST_CELL_ONLY_KEY: u16 = 16384;

/// `SPACE_DATA_FIRST_NON_USER_CELL_ONLY_KEY`.
pub const SPACE_DATA_FIRST_NON_USER_CELL_ONLY_KEY: u16 = SPACE_DATA_FINAL_USER_KEY + 1;

/// `SPACE_DATA_RECORDING`: the recording name entry.
pub const SPACE_DATA_RECORDING: u16 = SPACE_DATA_FIRST_NON_USER_CELL_ONLY_KEY;

/// `SPACE_DATA_ARTIFICIAL_LOAD`: the artificial-load payload, one float.
pub const SPACE_DATA_ARTIFICIAL_LOAD: u16 = SPACE_DATA_FIRST_NON_USER_CELL_ONLY_KEY + 1;

/// `SPACE_DATA_SERVER_LOAD_BOUNDS`.
pub const SPACE_DATA_SERVER_LOAD_BOUNDS: u16 = SPACE_DATA_FIRST_NON_USER_CELL_ONLY_KEY + 2;

/// `sizeof( SpaceData_MappingData )`: the `matrix[4][4]` that precedes the
/// path bytes in a geometry-mapping entry.
pub const SPACE_DATA_MAPPING_MATRIX_LEN: usize = 64;

/// `sizeof( SpaceData_ToDData )`.
pub const SPACE_DATA_TOD_LEN: usize = 8;

/// `sizeof( SpaceData_ArtificialLoad )`.
pub const SPACE_DATA_ARTIFICIAL_LOAD_LEN: usize = 4;

/// A stored space-data value (`SpaceDataMapping::DataValue`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DataValue {
    /// The entry's key, or [`NO_SPACE_DATA_KEY`] for a deleted entry.
    pub key: u16,
    /// The entry's payload.
    pub data: Vec<u8>,
}

impl DataValue {
    /// The deleted form of the value.
    pub fn none() -> Self {
        Self {
            key: NO_SPACE_DATA_KEY,
            data: Vec::new(),
        }
    }

    /// `valid()`: an entry that still names a key.
    pub fn valid(&self) -> bool {
        self.key != NO_SPACE_DATA_KEY
    }
}

/// `SpaceDataMapping` (`lib/network/space_data_mapping.cpp`): the one
/// entry per `SpaceEntryID` a space holds.
#[derive(Clone, Debug, Default)]
pub struct SpaceDataMapping {
    entries: BTreeMap<u32, DataValue>,
}

impl SpaceDataMapping {
    /// `clear()`.
    pub fn clear(&mut self) {
        self.entries.clear();
    }

    /// `addDataEntry`: the entry is added only when it is novel — an id
    /// the space already holds stays untouched and the call reports false.
    pub fn add_data_entry(&mut self, entry_id: u32, key: u16, data: Vec<u8>) -> bool {
        if self.entries.contains_key(&entry_id) {
            return false;
        }
        self.entries.insert(entry_id, DataValue { key, data });
        true
    }

    /// `delDataEntry( id )`.
    pub fn del_data_entry(&mut self, entry_id: u32) -> bool {
        self.entries.remove(&entry_id).is_some()
    }

    /// `delDataEntry( id, oldValue )`: the deleted value comes back, or
    /// the invalid form when there was no such entry.
    pub fn del_data_entry_value(&mut self, entry_id: u32) -> (bool, DataValue) {
        match self.entries.remove(&entry_id) {
            Some(value) => (true, value),
            None => (false, DataValue::none()),
        }
    }

    /// `dataRetrieveSpecific`: the entry, or the invalid form.
    pub fn data_retrieve_specific(&self, entry_id: u32) -> DataValue {
        self.entries
            .get(&entry_id)
            .cloned()
            .unwrap_or_else(DataValue::none)
    }

    /// `dataRetrieveSpecificPtr`: the entry's value, or nothing.
    pub fn data_retrieve_specific_ptr(&self, entry_id: u32) -> Option<&DataValue> {
        self.entries.get(&entry_id)
    }

    /// `dataRetrieveFirst`: the first entry carrying that key, or nothing.
    pub fn data_retrieve_first(&self, key: u16) -> Option<&DataValue> {
        self.entries.values().find(|value| value.key == key)
    }

    /// `size`.
    pub fn size(&self) -> usize {
        self.entries.len()
    }

    /// No entries at all.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// `begin`/`end`: the entries in order.
    pub fn entries(&self) -> impl Iterator<Item = (u32, &DataValue)> + '_ {
        self.entries.iter().map(|(id, value)| (*id, value))
    }
}

/// One cell of a Space (`CellInfo` in the space's tree).
#[derive(Clone, Debug, PartialEq)]
pub struct CellInfo {
    /// The space this cell partitions.
    pub space_id: SpaceId,
    /// The CellApp this cell is assigned to (`CellApp::interface().address()`).
    pub app_addr: u32,
    /// The cell's tree identity.
    pub node: CellInfoNode,
    /// `rect()`: the cell's partition rectangle, `(minX, minZ, maxX,
    /// maxZ)`. The ghost maintainer intersects it against the real's
    /// hysteresis area; `pCellAt` contains-checks it.
    pub rect: (f32, f32, f32, f32),
    /// `isDeletePending()`: the cell is being deleted; no ghosts are
    /// created on it and it is no offload destination.
    pub is_delete_pending: bool,
}

impl CellInfo {
    /// `rect().contains( x, z )`: the cell owns this point.
    pub fn contains(&self, x: f32, z: f32) -> bool {
        let (min_x, min_z, max_x, max_z) = self.rect;
        x >= min_x && x <= max_x && z >= min_z && z <= max_z
    }

    /// `Rect::intersects`: any overlap with the other
    /// `(minX, minZ, maxX, maxZ)` rectangle.
    pub fn intersects(&self, other: (f32, f32, f32, f32)) -> bool {
        let (min_x, min_z, max_x, max_z) = self.rect;
        let (o_min_x, o_min_z, o_max_x, o_max_z) = other;
        min_x <= o_max_x && max_x >= o_min_x && min_z <= o_max_z && max_z >= o_min_z
    }
}

/// The identity a `Cell`'s info holds; [`Space::has_single_cell`] compares
/// it against the space's tree root, exactly as
/// `((SpaceNode*)&pCell_->cellInfo() == pCellInfoTree_)` does.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct CellInfoNode(usize);

impl CellInfoNode {
    /// The first tree identity (`pCellInfoTree_`'s root).
    pub fn first() -> Self {
        CellInfoNode(0)
    }

    /// The next tree identity.
    pub fn next(value: CellInfoNode) -> Self {
        CellInfoNode(value.0 + 1)
    }
}

/// `Cell`: the CellApp's own cell for a Space, the one holding the space's
/// real entities.
#[derive(Clone, Debug)]
pub struct Cell {
    /// `cellInfo()`.
    pub info: CellInfo,
    /// How many `onSpaceGone` deliveries this cell has had.
    pub space_gone_count: u32,
    /// How many `Cell::reuse` deliveries this cell has had ("Cell did not
    /// fully remove; reusing.").
    pub reuse_count: u32,
    /// `shouldOffload_` (the constructor's `true`): the check drives
    /// offloads and ghost maintenance for this cell.
    pub should_offload: bool,
    /// `isRemoved_`: the cell has been removed and may be deleted once
    /// empty (`isReadyForDeletion`).
    pub is_removed: bool,
}

impl Cell {
    /// `Cell::reuse`: the cell did not fully remove before the manager
    /// re-added it; the same cell object carries on.
    pub fn reuse(&mut self) {
        self.reuse_count += 1;
    }
}

/// `Entity`: the space's own entry for one of its entities — the removal
/// handle bookkeeping, the real/ghost flag the destructor turns on, and the
/// creation state `Entity::initReal` streamed in (`entity.cpp`).
#[derive(Clone, Debug)]
pub struct Entity {
    /// The entity id.
    pub id: u32,
    /// `removalHandle_`: the index this entity sits at in its space's
    /// entity list, or [`NO_SPACE_REMOVAL_HANDLE`] when none holds it.
    pub removal_handle: usize,
    /// The id of the space holding this entity (`pCell_->space().id()` on
    /// the reference). Written by [`Space::add_entity`]; only read while
    /// the removal handle says a space holds it.
    pub space_id: SpaceId,
    /// `isReal()`.
    pub is_real: bool,
    /// `isDestroyed()`.
    pub destroyed: bool,
    /// `entityTypeID_`: the def type this entity was created as.
    pub type_id: u32,
    /// `localPosition_` as created (`initReal` reads it first).
    pub position: [f32; 3],
    /// `localDirection_`: yaw, pitch, roll.
    pub direction: [f32; 3],
    /// `isOnGround_`, from the `AddEntityData` flag.
    pub is_on_ground: bool,
    /// `lastEventNumber_`.
    pub last_event_number: u32,
    /// `lastTrimmedEventNumber()`: the oldest event the client-facing
    /// history still holds; a witness cache re-entering with a stamp
    /// older than this takes the refresh path (`EntityCache::reuse`).
    pub last_trimmed_event_number: u32,
    /// `volatileUpdateNumber()`: the volatile update counter the
    /// witness caches restamp from on reset (their `lastVolatile` is
    /// this minus one).
    pub volatile_update_number: u16,
    /// `propertyOwner_`'s ghost half: the property data
    /// `ghostedDataUpdate` set, by the type's property index.
    pub ghost_properties: BTreeMap<i32, Vec<u8>>,
    /// `nextRealAddr_`: the address the entity's real is moving to (a
    /// real mid-offload names its destination; a ghost holds the
    /// sign-off it received until the new real's first message).
    pub next_real_addr: Option<u32>,
    /// `baseAddr_`: the base's internal address, when the stream carried one.
    pub base_addr: Option<(u32, u16)>,
    /// The Witness indicator byte `RealEntity::init` read (`'W'`/`'-'`).
    pub has_witness: bool,
    /// `pControllers_`: the entity's attached controllers.
    pub controllers: crate::cellapp_real_entity::Controllers,
    /// `extras_`: the `EntityExtra` slot array, indexed by eeid. Empty
    /// until an extra is touched or the slots are sized to the registry.
    pub extras: Vec<Option<crate::cellapp_real_entity::ExtraSlot>>,
    /// `RealEntity::velocity_`: the sampled velocity
    /// (`RealEntity::newPosition`), read by the acceleration controllers.
    pub velocity: [f32; 3],
    /// `RealEntity::positionSample_`: where the velocity sampler last
    /// took a position (`owner.position()` at construction).
    pub position_sample: [f32; 3],
    /// `RealEntity::positionSampleTime_`: the game tick of that sample.
    pub position_sample_time: u32,
    /// `pVehicle_` (`entity.cpp:7606`): the vehicle this entity rides,
    /// when any. `None` is also the alighted-with-`KEEP_GLOBAL_POSITION`
    /// state — the limbo face keeps it `None` with the passenger extra
    /// and controller still live.
    pub vehicle: Option<u32>,
    /// `vehicleChangeNum_`: bumped on every vehicle change (the witness
    /// dedups its vehicleChanged sends against it, BW-08-03).
    pub vehicle_change_num: u32,
    /// `localPosition_`: this entity's position in its vehicle's frame,
    /// or in the global frame when unmounted (the invariant
    /// `updateLocalPosition`/`updateGlobalPosition` maintain).
    pub local_position: [f32; 3],
    /// `localDirection_`: yaw, pitch, roll in the same frame.
    pub local_direction: [f32; 3],
    /// `RealEntity::controlledByRef().id`: the client controlling this
    /// entity, when any — the face `sendPhysicsCorrection` gates on.
    pub controlled_by: u32,
    /// `BaseAccelerationController::pVehicle_`: the vehicle the
    /// acceleration controller's velocity sampler last saw (the
    /// entity can carry at most one — the family is exclusive).
    pub accel_vehicle: Option<u32>,
    /// `BaseAccelerationController::positionSample_` (the controller's
    /// own, in local space — distinct from the RealEntity sample).
    pub accel_sample: [f32; 3],
    /// `BaseAccelerationController::positionSampleTime_`.
    pub accel_sample_time: u32,
    /// `BaseAccelerationController::currVelocity_` (local space).
    pub accel_velocity: [f32; 3],
    /// `inDestroy_` (`entity.cpp:2898`): the destroy latch — reentrant
    /// and repeat destroys are inert until `setToInitialState` (the
    /// revive) clears it.
    pub in_destroy: bool,
    /// `pRangeListNode_`: this entity's node in its space's range list
    /// (`None` while no space holds it — the reference's node lives as
    /// long as the entity, the port's arena owns its slot).
    pub range_node: Option<crate::cellapp_range_list::NodeId>,
    /// `triggers_` (`entity.hpp`): this entity's own range triggers,
    /// sorted by range, largest first.
    pub range_triggers: Vec<crate::cellapp_range_list::TriggerId>,
    /// `eventHistory_`: the events the witnesses' caches replay from.
    pub event_history: crate::cellapp_witness::EventHistory,
    /// `periodsWithoutWitness_`: the unwitnessed tally the send pass
    /// clears and the tick increments.
    pub periods_without_witness: u32,
    /// `EntityDescription::volatileInfo()`: the per-type volatile
    /// priorities this entity's projections gate on.
    pub volatile_info: crate::cellapp_witness::VolatileInfo,
    /// The `CellAppInterface::witnessed` sends a ghost's witnessed arm
    /// made: the ghost ids the real was told about.
    pub witnessed_sends: Vec<u32>,
    /// `RealEntity::creationTime()`: the tick the real was created at —
    /// the ghost maintainer keeps ghosts of brand-new reals.
    pub creation_time: u32,
    /// `pRealChannel_`'s address on a ghost: where the real lives
    /// (`readGhostDataFromStreamInternal`), `None` while real or unset.
    pub real_addr: Option<u32>,
    /// `numTimesRealOffloaded_`: the offload tally the ghost stream
    /// carries (`BufferedGhostMessage` also reads it off the head).
    pub num_times_real_offloaded: u32,
    /// `aoiUpdateSchemeID_` (uint8).
    pub aoi_update_scheme_id: u8,
    /// `RealEntity::haunts_`: the ghosts this real maintains, per remote
    /// CellApp address, with each haunt's creation tick.
    pub haunts: Vec<crate::cellapp_ghost::Haunt>,
}

impl Entity {
    /// An entity no space holds yet, with the handle reset and the real
    /// state at its defaults (`setToInitialState`).
    pub fn new(id: u32, is_real: bool) -> Self {
        Self {
            id,
            removal_handle: NO_SPACE_REMOVAL_HANDLE,
            space_id: 0,
            is_real,
            destroyed: false,
            in_destroy: false,
            type_id: 0,
            position: [0.0; 3],
            direction: [0.0; 3],
            is_on_ground: false,
            last_event_number: 0,
            last_trimmed_event_number: 0,
            volatile_update_number: 0,
            ghost_properties: BTreeMap::new(),
            next_real_addr: None,
            base_addr: None,
            has_witness: false,
            controllers: crate::cellapp_real_entity::Controllers::default(),
            extras: Vec::new(),
            velocity: [0.0; 3],
            position_sample: [0.0; 3],
            position_sample_time: 0,
            vehicle: None,
            vehicle_change_num: 0,
            local_position: [0.0; 3],
            local_direction: [0.0; 3],
            controlled_by: 0,
            accel_vehicle: None,
            event_history: crate::cellapp_witness::EventHistory::default(),
            periods_without_witness: crate::cellapp_witness::NOT_WITNESSED_THRESHOLD,
            volatile_info: crate::cellapp_witness::VolatileInfo::default(),
            witnessed_sends: Vec::new(),
            accel_sample: [0.0; 3],
            accel_sample_time: 0,
            accel_velocity: [0.0; 3],
            range_node: None,
            range_triggers: Vec::new(),
            creation_time: 0,
            real_addr: None,
            num_times_real_offloaded: 0,
            aoi_update_scheme_id: 0,
            haunts: Vec::new(),
        }
    }
}

/// `Space::UpdateCellAppMgr`: whether `spaceDataEntry` forwards the entry
/// to the CellAppMgr itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UpdateCellAppMgr {
    /// `UPDATE_CELL_APP_MGR`: forward it (the default).
    Update,
    /// `DONT_UPDATE_CELL_APP_MGR`: it arrived from the manager or a peer.
    DontUpdate,
}

/// `Space::DataEffected`: whether `spaceDataEntry` must still act on the
/// entry (load geometry, set the time of day) or that already happened.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DataEffected {
    /// `ALREADY_EFFECTED`.
    AlreadyEffected,
    /// `NEED_TO_EFFECT` (the default).
    NeedToEffect,
}

/// `ISpaceWorldFactory` — the game-delegate world factory, recording the
/// calls a [`PhysicalDelegateSpace`] makes into it so the mount and the
/// unmount stay observable after the space itself is gone.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SpaceWorldFactory {
    /// `LoadWorld` calls.
    pub loaded: Vec<String>,
    /// `RemoveWorld` calls.
    pub removed: Vec<String>,
    /// `CreateWorldInstance` calls.
    pub created: Vec<String>,
    /// `RemoveWorldInstance` calls.
    pub removed_instances: Vec<String>,
    /// `CancelLoadingWorld` calls.
    pub cancelled: Vec<String>,
    /// `CreateSpacePhysicsDelegate` calls.
    pub physics_delegates_created: u32,
    /// `RemoveSpacePhysicsDelegate` calls — the per-space physics delegate
    /// is released only in the physical space's destructor.
    pub physics_delegates_removed: u32,
    /// The paths `IsLoadingWorldFailed` reports true for.
    pub failed: Vec<String>,
}

impl SpaceWorldFactory {
    /// `CreateSpacePhysicsDelegate`.
    pub fn create_physics_delegate(&mut self) {
        self.physics_delegates_created += 1;
    }

    /// `RemoveSpacePhysicsDelegate`.
    pub fn remove_physics_delegate(&mut self) {
        self.physics_delegates_removed += 1;
    }

    /// `LoadWorld`.
    pub fn load_world(&mut self, path: &str) {
        self.loaded.push(path.to_string());
    }

    /// `CreateWorldInstance`: succeeds once the world's resources are in —
    /// a path marked failed never succeeds.
    pub fn create_world_instance(&mut self, path: &str) -> bool {
        self.created.push(path.to_string());
        !self.failed.iter().any(|failed| failed == path)
    }

    /// `RemoveWorld`.
    pub fn remove_world(&mut self, path: &str) {
        self.removed.push(path.to_string());
    }

    /// `RemoveWorldInstance`.
    pub fn remove_world_instance(&mut self, path: &str) {
        self.removed_instances.push(path.to_string());
    }

    /// `CancelLoadingWorld`.
    pub fn cancel_loading_world(&mut self, path: &str) {
        self.cancelled.push(path.to_string());
    }

    /// `IsLoadingWorldFailed`.
    pub fn is_loading_world_failed(&self, path: &str) -> bool {
        self.failed.iter().any(|failed| failed == path)
    }

    /// A test seam: mark a path's load as failed.
    pub fn fail_world(&mut self, path: &str) {
        self.failed.push(path.to_string());
    }
}

/// A geometry mapping of the chunk backend — one `EdgeGeometryMappings`
/// entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GeometryMapping {
    /// The resource path of the mapping.
    pub path: String,
    /// `addMappingAsync` is still in flight: the mapping only becomes
    /// loaded when a [`PhysicalSpace::update`] tick advances it.
    pub pending: bool,
}

/// `ChunkSpace` — only the faces the unmount touches. The real `ChunkSpace`
/// lives with the chunk lane; what carries over here is the tag, the raw
/// mapping-factory back-reference into the owning backend, and `clear()`'s
/// mark.
#[derive(Debug)]
pub struct ChunkSpace {
    /// The `ChunkSpace( id )` tag.
    pub id: SpaceId,
    /// `pMappingFactory_ != NULL`: the raw, non-owning back-reference into
    /// the owning `PhysicalChunkSpace`, which only `clear()` severs
    /// (`physical_chunk_space.cpp:184`).
    pub mapping_factory_attached: bool,
    /// `cleared_`.
    pub cleared: bool,
}

/// `PhysicalChunkSpace` (`physical_chunk_space.cpp`): chunked geometry
/// through a `ChunkSpace` this backend owns.
#[derive(Debug)]
pub struct PhysicalChunkSpace {
    /// The `ChunkSpace` created in the constructor and pointed at this
    /// backend's mapping factory (`:63-69`).
    pub chunk_space: ChunkSpace,
    /// `geometryMappings_`: one entry per `SpaceEntryID`.
    pub mappings: BTreeMap<u32, GeometryMapping>,
}

impl PhysicalChunkSpace {
    /// `PhysicalChunkSpace( id, mapper )`.
    pub fn new(id: SpaceId) -> Self {
        Self {
            chunk_space: ChunkSpace {
                id,
                mapping_factory_attached: true,
                cleared: false,
            },
            mappings: BTreeMap::new(),
        }
    }

    /// `loadResource` → `addMappingAsync`: the mapping arrives pending.
    /// The matrix (the entry's `SpaceData_MappingData`) positions the
    /// mapping and belongs to the chunk lane, so it is not carried here.
    pub fn load_resource(&mut self, entry_id: u32, path: &str) {
        self.mappings.insert(
            entry_id,
            GeometryMapping {
                path: path.to_string(),
                pending: true,
            },
        );
    }

    /// `unloadResource` → `delMapping`.
    pub fn unload_resource(&mut self, entry_id: u32) {
        self.mappings.remove(&entry_id);
    }

    /// `update` → `tickLoading`: pending mappings advance unless the tick
    /// is unload-only. The return stands in for `anyColumnsLoaded`:
    /// whether any mounted mapping is loaded.
    pub fn update(&mut self, unload_only: bool) -> bool {
        if !unload_only {
            for mapping in self.mappings.values_mut() {
                mapping.pending = false;
            }
        }
        self.mappings.values().any(|mapping| !mapping.pending)
    }

    /// `isFullyUnloaded`: no mappings left.
    pub fn is_fully_unloaded(&self) -> bool {
        self.mappings.is_empty()
    }

    /// `reuse` (`:172-175`): the mappings drain through the chunk space's
    /// own `clear()` — the mapping factory stays attached.
    pub fn reuse(&mut self) {
        self.mappings.clear();
        self.chunk_space.cleared = true;
    }

    /// `clear` (`:181-185`): the mappings drain **and** the
    /// mapping-factory back-reference is severed.
    pub fn clear(&mut self) {
        self.mappings.clear();
        self.chunk_space.cleared = true;
        self.chunk_space.mapping_factory_attached = false;
    }
}

/// `PhysicalDelegateSpace` (`physical_delegate_space.cpp`): worlds through
/// the game-delegate factory.
#[derive(Debug)]
pub struct PhysicalDelegateSpace {
    /// `id_`.
    pub id: SpaceId,
    /// `paths_`: the mounted world resources, one per `SpaceEntryID`.
    pub paths: BTreeMap<u32, String>,
    /// `worlds_`: the materialised instances (holding the path once
    /// `CreateWorldInstance` succeeded).
    pub worlds: BTreeMap<u32, String>,
    /// The factory every call goes through — shared with the holder, so
    /// the releases survive the space.
    factory: Rc<RefCell<SpaceWorldFactory>>,
}

impl PhysicalDelegateSpace {
    /// `PhysicalDelegateSpace( id )`: the per-space physics delegate is
    /// created here and released only in the destructor (`:14-30`).
    pub fn new(id: SpaceId, factory: Rc<RefCell<SpaceWorldFactory>>) -> Self {
        factory.borrow_mut().create_physics_delegate();
        Self {
            id,
            paths: BTreeMap::new(),
            worlds: BTreeMap::new(),
            factory,
        }
    }

    /// `loadResource` (`:57-74`): an entry that already carries a path is
    /// reused with an INFO and no factory call; otherwise the world's
    /// resources are loaded and the path recorded.
    pub fn load_resource(&mut self, entry_id: u32, path: &str) {
        if self.paths.contains_key(&entry_id) {
            return; // "Reusing mapping %s for entry id %s"
        }
        self.factory.borrow_mut().load_world(path);
        self.paths.insert(entry_id, path.to_string());
    }

    /// `unloadResource` (`:79-106`): an unknown entry only logs; otherwise
    /// the world instance goes first, then the world's resources.
    pub fn unload_resource(&mut self, entry_id: u32) {
        let Some(path) = self.paths.remove(&entry_id) else {
            return; // "Cannot find mapping for entry id %s"
        };
        if self.worlds.remove(&entry_id).is_some() {
            self.factory.borrow_mut().remove_world_instance(&path);
        }
        self.factory.borrow_mut().remove_world(&path);
    }

    /// `update` (`:112-158`): materialise a world instance for every path
    /// not yet loaded; a failed load erases the path and ends this pass.
    /// Always returns false — the reference's load balancing is a TODO.
    pub fn update(&mut self, _unload_only: bool) -> bool {
        for id in self.paths.keys().copied().collect::<Vec<u32>>() {
            if self.worlds.contains_key(&id) {
                continue;
            }
            let path = self.paths[&id].clone();
            let mut factory = self.factory.borrow_mut();
            if factory.create_world_instance(&path) {
                self.worlds.insert(id, path);
            } else {
                // CreateWorldInstance returned NULL. A failed load drops
                // the path and ends this pass — the healthy path waits for
                // the next update, like the reference's break out of a
                // modified paths_ map; a world still loading retries on
                // the next update.
                debug_assert!(factory.is_loading_world_failed(&path));
                self.paths.remove(&id);
                return false;
            }
        }
        false
    }

    /// `isFullyUnloaded`: no paths left.
    pub fn is_fully_unloaded(&self) -> bool {
        self.paths.is_empty()
    }

    /// `reuse` (`:267-270`): a plain `clear()`.
    pub fn reuse(&mut self) {
        self.clear();
    }

    /// `clear` (`:276-298`): cancel the in-flight loads, remove every
    /// world instance, then every world's resources. Idempotent.
    pub fn clear(&mut self) {
        for (id, path) in &self.paths {
            if !self.worlds.contains_key(id) {
                self.factory.borrow_mut().cancel_loading_world(path);
            }
        }
        for (_, path) in std::mem::take(&mut self.worlds) {
            self.factory.borrow_mut().remove_world_instance(&path);
        }
        for (_, path) in std::mem::take(&mut self.paths) {
            self.factory.borrow_mut().remove_world(&path);
        }
    }
}

impl PhysicalSpace {
    /// The chunk backend, when this is one.
    pub fn as_chunk(&self) -> Option<&PhysicalChunkSpace> {
        match self {
            PhysicalSpace::Chunk(chunk) => Some(chunk),
            PhysicalSpace::Delegate(_) => None,
        }
    }

    /// The delegate backend, when this is one.
    pub fn as_delegate(&self) -> Option<&PhysicalDelegateSpace> {
        match self {
            PhysicalSpace::Chunk(_) => None,
            PhysicalSpace::Delegate(space) => Some(space),
        }
    }

    /// The backend dispatch `Space` drives: `loadResource`.
    pub fn load_resource(&mut self, entry_id: u32, path: &str) {
        match self {
            PhysicalSpace::Chunk(chunk) => chunk.load_resource(entry_id, path),
            PhysicalSpace::Delegate(space) => space.load_resource(entry_id, path),
        }
    }

    /// `unloadResource`.
    pub fn unload_resource(&mut self, entry_id: u32) {
        match self {
            PhysicalSpace::Chunk(chunk) => chunk.unload_resource(entry_id),
            PhysicalSpace::Delegate(space) => space.unload_resource(entry_id),
        }
    }

    /// `update( rect, unloadOnly )`.
    pub fn update(&mut self, unload_only: bool) -> bool {
        match self {
            PhysicalSpace::Chunk(chunk) => chunk.update(unload_only),
            PhysicalSpace::Delegate(space) => space.update(unload_only),
        }
    }

    /// `isFullyUnloaded`.
    pub fn is_fully_unloaded(&self) -> bool {
        match self {
            PhysicalSpace::Chunk(chunk) => chunk.is_fully_unloaded(),
            PhysicalSpace::Delegate(space) => space.is_fully_unloaded(),
        }
    }

    /// `reuse`.
    pub fn reuse(&mut self) {
        match self {
            PhysicalSpace::Chunk(chunk) => chunk.reuse(),
            PhysicalSpace::Delegate(space) => space.reuse(),
        }
    }

    /// `clear`.
    pub fn clear(&mut self) {
        match self {
            PhysicalSpace::Chunk(chunk) => chunk.clear(),
            PhysicalSpace::Delegate(space) => space.clear(),
        }
    }
}

/// `IPhysicalSpace`: the geometry backend a [`Space`] mounts. The reference
/// picks the implementation in the `Space` constructor — a
/// `PhysicalDelegateSpace` when a game delegate is installed, a
/// `PhysicalChunkSpace` otherwise (`space.cpp:79-86`).
#[derive(Debug)]
pub enum PhysicalSpace {
    /// The chunked backend.
    Chunk(PhysicalChunkSpace),
    /// The game-delegate backend.
    Delegate(PhysicalDelegateSpace),
}

/// `PhysicalSpacePtr`: `SmartPointer< IPhysicalSpace >`. The `Space` that
/// mounted the backend is its only holder, so releasing that one reference
/// at `~Space` destroys the backend.
pub type PhysicalSpacePtr = Rc<RefCell<PhysicalSpace>>;

impl Drop for PhysicalSpace {
    fn drop(&mut self) {
        match self {
            // `~EdgeGeometryMappings` asserts the mappings were drained
            // (`edge_geometry_mappings.cpp:26-30`) — which is what makes
            // the explicit `clear()` in `~Space` mandatory.
            PhysicalSpace::Chunk(chunk) => debug_assert!(
                chunk.mappings.is_empty(),
                "the chunk backend died with mappings still attached"
            ),
            // `~PhysicalDelegateSpace` (`:44-49`): `clear()` again
            // (idempotent), then the per-space physics delegate — created
            // at mount — is released, and only here.
            PhysicalSpace::Delegate(space) => {
                space.clear();
                space.factory.borrow_mut().remove_physics_delegate();
            }
        }
    }
}

/// `Space` (`space.cpp`): one space of the world as the CellApp holds it.
#[derive(Debug)]
pub struct Space {
    /// `id_`.
    pub id: SpaceId,
    /// `pCell_`: the cell holding this space's real entities.
    cell: Option<Rc<RefCell<Cell>>>,
    /// `cellInfos_`: every cell this space spans, tree order.
    cells: Vec<CellInfo>,
    /// `entities_`.
    entities: Vec<Rc<RefCell<Entity>>>,
    /// When the `ShutdownSpace` timer was armed, or `None` while it is
    /// unset — `shuttingDownTimerHandle_.isSet()`.
    timer_armed_at: Option<u64>,
    /// `begDataSeq_`.
    pub beg_data_seq: u64,
    /// `endDataSeq_`.
    pub end_data_seq: u64,
    /// `initialTimeOfDay_`, the reference's `-1.f` unset sentinel.
    pub initial_time_of_day: f32,
    /// `gameSecondsPerSecond_`.
    pub game_seconds_per_second: f32,
    /// `spaceDataMapping_`.
    pub space_data: SpaceDataMapping,
    /// `recentData_`: the `(entryID, key)` entries recorded since the last
    /// reuse — user-visible keys only, adds and revocations alike, most
    /// recent last.
    pub recent_data: Vec<(u32, u16)>,
    /// `pPhysicalSpace_`: the mounted geometry backend. The space is its
    /// only holder; [`Space::destroy`] releases that one reference.
    physical: Option<PhysicalSpacePtr>,
    /// `shouldCheckLoadBounds_`, set by `onNewGeometryMapping`.
    pub should_check_load_bounds: bool,
    /// `lastMappedGeometry_`.
    pub last_mapped_geometry: String,
    /// `artificialMinLoad_`.
    pub artificial_min_load: f32,
}

impl Space {
    /// `Space( SpaceID id )`: no cell, no entities, no timer, the data
    /// faces at the constructor's own defaults, and a `PhysicalChunkSpace`
    /// mounted — the reference's no-game-delegate branch
    /// (`space.cpp:83-86`).
    pub fn new(id: SpaceId) -> Self {
        Self::with_physical(
            id,
            PhysicalSpacePtr::new(RefCell::new(PhysicalSpace::Chunk(PhysicalChunkSpace::new(
                id,
            )))),
        )
    }

    /// The reference's game-delegate branch (`space.cpp:79-82`): a
    /// `PhysicalDelegateSpace` over the given world factory.
    pub fn new_delegate(id: SpaceId, factory: Rc<RefCell<SpaceWorldFactory>>) -> Self {
        Self::with_physical(
            id,
            PhysicalSpacePtr::new(RefCell::new(PhysicalSpace::Delegate(
                PhysicalDelegateSpace::new(id, factory),
            ))),
        )
    }

    /// `MF_ASSERT( pPhysicalSpace_ )` done: the constructed space over the
    /// backend the caller mounted.
    pub fn with_physical(id: SpaceId, physical: PhysicalSpacePtr) -> Self {
        Self {
            id,
            cell: None,
            cells: Vec::new(),
            entities: Vec::new(),
            timer_armed_at: None,
            beg_data_seq: 0,
            end_data_seq: 0,
            initial_time_of_day: -1.0,
            game_seconds_per_second: 0.0,
            space_data: SpaceDataMapping::default(),
            recent_data: Vec::new(),
            physical: Some(physical),
            should_check_load_bounds: false,
            last_mapped_geometry: String::new(),
            artificial_min_load: 0.0,
        }
    }

    /// `setLastMappedGeometry`.
    pub fn set_last_mapped_geometry(&mut self, geometry: String) {
        self.last_mapped_geometry = geometry;
    }

    /// `dataBySeq( seq, entryID, key )`: the entry the sequence names,
    /// and the value still in the mapping. A revoked key hands back no
    /// value — the reference returns `NULL` then, too.
    pub fn data_by_seq(&self, seq: u32) -> (u32, u16, Option<&Vec<u8>>) {
        debug_assert!(self.beg_data_seq <= seq as u64);
        debug_assert!((seq as u64) < self.end_data_seq);
        let (entry_id, key) = self.recent_data[(seq as usize) - self.beg_data_seq as usize];
        let blob = (key != NO_SPACE_DATA_KEY)
            .then(|| self.space_data.data_retrieve_specific_ptr(entry_id))
            .flatten()
            .filter(|value| value.key != NO_SPACE_DATA_KEY)
            .map(|value| &value.data);
        (entry_id, key, blob)
    }

    /// `pPhysicalSpace()`: the mounted backend. The accessor hands out a
    /// clone of the pointer, like the reference's by-value
    /// `SmartPointer` return — the space stays the only *persistent*
    /// holder.
    pub fn physical(&self) -> Option<PhysicalSpacePtr> {
        self.physical.clone()
    }

    /// `isFullyUnloaded` (`space.cpp:1230`): nothing in the space and the
    /// backend fully drained — the gate `deleteOldSpaces` deletes through.
    pub fn is_fully_unloaded(&self) -> bool {
        if !self.entities.is_empty() {
            return false;
        }
        match &self.physical {
            Some(physical) => physical.borrow().is_fully_unloaded(),
            None => true,
        }
    }

    /// `chunkTick`'s physical half (`space.cpp:1140`): the update rect is
    /// driven around the cell while there is one — with no cell the tick
    /// runs unload-only, so nothing new loads and the mounted resources
    /// drain.
    pub fn chunk_tick(&mut self) -> bool {
        let unload_only = self.cell.is_none();
        match &self.physical {
            Some(physical) => physical.borrow_mut().update(unload_only),
            None => false,
        }
    }

    /// `Space::reuse`: the space data faces reset, the cell-info tree is
    /// deleted, and the mounted backend reuses (`space.cpp:156`) — the
    /// cell and the entities stay where they are.
    pub fn reuse(&mut self) {
        self.beg_data_seq = 0;
        self.end_data_seq = 0;
        self.initial_time_of_day = -1.0;
        self.game_seconds_per_second = 0.0;
        self.cells.clear();
        self.space_data.clear();
        self.recent_data.clear();
        if let Some(physical) = &self.physical {
            physical.borrow_mut().reuse();
        }
    }

    /// `updateGeometry` (`space.cpp:885`): every cell of the old tree is
    /// marked for deletion, the tree is rebuilt from the message, the marks
    /// are resolved, the space's own cell is either announced as going or
    /// checked off, and a space that collapsed from several cells to one
    /// gets another chance at the shutdown. The wire `readTree` (bounds,
    /// portals) is not decoded here — the tree arrives already parsed as
    /// [`CellInfo`].
    pub fn update_geometry(
        &mut self,
        cell_app: &mut CellApp,
        tree: Vec<CellInfo>,
    ) -> TopologyReport {
        let mut report = TopologyReport::default();
        let was_multicell = !self.has_single_cell();

        // Every old cell is marked; the rebuilt tree is read from the
        // message, and the marks are resolved against it.
        let old = self.cells.clone();
        self.cells = tree;
        let deleted: Vec<CellInfo> = old
            .iter()
            .filter(|info| {
                !self
                    .cells
                    .iter()
                    .any(|kept| kept.node == info.node && kept.app_addr == info.app_addr)
            })
            .cloned()
            .collect();
        report.deleted = deleted.iter().map(|info| info.node).collect();

        let own_gone = match self.cell() {
            Some(cell) => {
                let node = cell.borrow().info.node;
                deleted.iter().any(|info| info.node == node)
            }
            None => false,
        };
        report.own_cell_gone = own_gone;
        report.cell_checked_off = self.cell().is_some() && !own_gone;

        // see if we want to expressly shut down this space now
        if was_multicell {
            report.checked_for_shut_down = true;
            report.requested_shut_down = self.check_for_shut_down(cell_app);
        }
        report
    }

    /// `findCell( address )`: the tree's cell assigned to this CellApp,
    /// if any.
    pub fn find_cell_info(&self, app_addr: u32) -> Option<CellInfo> {
        self.cells
            .iter()
            .find(|info| info.app_addr == app_addr)
            .cloned()
    }

    /// `allSpaceData` / `readDataFromStream`: the manager's full space-data
    /// payload, each entry of it going through [`Space::space_data_entry`]
    /// without touching the manager and still needing to be effected. An
    /// empty payload leaves the mapping untouched (the reference only
    /// reads it when `remainingLength() > 0`).
    pub fn read_data_from_stream(
        &mut self,
        cell_app: &mut CellApp,
        entries: Vec<(u32, u16, Vec<u8>)>,
    ) {
        if entries.is_empty() {
            return;
        }
        for (entry_id, key, data) in entries {
            self.space_data_entry(
                cell_app,
                entry_id,
                key,
                data,
                UpdateCellAppMgr::DontUpdate,
                DataEffected::NeedToEffect,
            );
        }
    }

    /// `Space::spaceDataEntry` (`space.cpp:675`): an entry arriving for
    /// this space. The manager-bound branch only sends the message — both
    /// branches fall through to the mapping itself, so a forwarded entry
    /// is stored here too. The re-add guard and `recent_data` cover
    /// user-visible keys only (`< SPACE_DATA_FIRST_CELL_ONLY_KEY`; the
    /// delete key `uint16(-1)` is above every user key), a refused add or
    /// a delete of nothing ("attempted to add or delete entry twice!")
    /// returns false, and a `NEED_TO_EFFECT` call acts on the effective
    /// key: the geometry entry mounts into / unloads from the physical
    /// space, the time-of-day and artificial-load payloads land in their
    /// fields.
    pub fn space_data_entry(
        &mut self,
        cell_app: &mut CellApp,
        entry_id: u32,
        key: u16,
        data: Vec<u8>,
        action: UpdateCellAppMgr,
        effected: DataEffected,
    ) -> bool {
        if action == UpdateCellAppMgr::Update {
            // `b << id_ << entryID << key`, the value blob only when the
            // key is set; the replay record belongs to the replay face.
            cell_app.space_data_updates.push((self.id, entry_id, key));
        } else if key < SPACE_DATA_FIRST_CELL_ONLY_KEY {
            // make sure we are not trying to add back a recently deleted
            // key
            if self.recently_deleted(&entry_id, NO_SPACE_DATA_KEY) {
                cell_app.rejected_readds.push(entry_id);
                return false;
            }
        }

        let mut has_been_added = false;
        let mut effective_key = NO_SPACE_DATA_KEY;
        if key != NO_SPACE_DATA_KEY {
            has_been_added = self.space_data.add_data_entry(entry_id, key, data.clone());
            if has_been_added {
                effective_key = key;
            }
        } else {
            let (removed, old_value) = self.space_data.del_data_entry_value(entry_id);
            if removed {
                effective_key = old_value.key;
            }
        }

        // see if the chunk space already has this one — if it was not new
        // then effectiveKey is -1
        if effective_key == NO_SPACE_DATA_KEY {
            // "Attempted to add or delete entry twice!"
            return false;
        }

        // see if this is data that can be sent to the client; `rde.time`
        // is the CellApp's clock, which the client-sync face carries.
        if effective_key < SPACE_DATA_FIRST_CELL_ONLY_KEY {
            self.recent_data.push((entry_id, key));
            self.end_data_seq += 1;
            // `s_allSpacesDataChangeSeq_` is a free global in the
            // reference; the `CellApp` carries it here.
            cell_app.all_spaces_data_change_seq += 1;
        }

        // effect this change unless that has already been done
        if effected == DataEffected::NeedToEffect {
            match effective_key {
                SPACE_DATA_TOD_KEY => match self.space_data.data_retrieve_first(SPACE_DATA_TOD_KEY)
                {
                    Some(value) if value.data.len() >= SPACE_DATA_TOD_LEN => {
                        self.initial_time_of_day =
                            f32::from_le_bytes(value.data[0..4].try_into().expect("4 bytes"));
                        self.game_seconds_per_second =
                            f32::from_le_bytes(value.data[4..8].try_into().expect("4 bytes"));
                    }
                    _ => {
                        self.initial_time_of_day = -1.0;
                        self.game_seconds_per_second = 0.0;
                    }
                },
                SPACE_DATA_ARTIFICIAL_LOAD => {
                    match self
                        .space_data
                        .data_retrieve_first(SPACE_DATA_ARTIFICIAL_LOAD)
                    {
                        Some(value) if value.data.len() >= SPACE_DATA_ARTIFICIAL_LOAD_LEN => {
                            self.artificial_min_load =
                                f32::from_le_bytes(value.data[0..4].try_into().expect("4 bytes"));
                        }
                        _ => self.artificial_min_load = 0.0,
                    }
                }
                SPACE_DATA_MAPPING_KEY_CLIENT_SERVER => {
                    if has_been_added {
                        // see if this mapping is being added
                        assert!(
                            data.len() >= SPACE_DATA_MAPPING_MATRIX_LEN,
                            "a geometry payload is shorter than SpaceData_MappingData"
                        );
                        let path = String::from_utf8_lossy(&data[SPACE_DATA_MAPPING_MATRIX_LEN..]);
                        self.mount_resource(entry_id, &path);
                        // Someone else has mapped a geometry. If we thought
                        // we had the last mapped geometry then clear it.
                        self.last_mapped_geometry.clear();
                    }
                    // ok it's being removed then
                    else {
                        self.unmount_resource(entry_id);
                    }
                }
                // SPACE_DATA_RECORDING lands in `Cell::startRecording` —
                // the recording face; CLIENT_ONLY does nothing on the
                // server.
                _ => {}
            }
        }
        true
    }

    /// The geometry half of a `SPACE_DATA_MAPPING_KEY_CLIENT_SERVER` add:
    /// `pPhysicalSpace_->loadResource(...)` plus `onNewGeometryMapping`.
    fn mount_resource(&mut self, entry_id: u32, path: &str) {
        if let Some(physical) = &self.physical {
            physical.borrow_mut().load_resource(entry_id, path);
        }
        self.should_check_load_bounds = true;
    }

    /// `pPhysicalSpace_->unloadResource( entryID )`.
    fn unmount_resource(&mut self, entry_id: u32) {
        if let Some(physical) = &self.physical {
            physical.borrow_mut().unload_resource(entry_id);
        }
    }

    /// The `recentData_` scan: was this entry deleted with `key == -1`,
    /// and has nothing newer been recorded since?
    fn recently_deleted(&self, entry_id: &u32, key: u16) -> bool {
        self.recent_data
            .iter()
            .rev()
            .any(|(id, recorded_key)| id == entry_id && *recorded_key == key)
    }

    /// `numEntities`.
    pub fn num_entities(&self) -> usize {
        self.entities.len()
    }

    /// `numCells`.
    pub fn num_cells(&self) -> usize {
        self.cells.len()
    }

    /// The space's entities.
    pub fn entities(&self) -> &[Rc<RefCell<Entity>>] {
        &self.entities
    }

    /// `pCell()`.
    pub fn cell(&self) -> Option<&Rc<RefCell<Cell>>> {
        self.cell.as_ref()
    }

    /// `pCell( Cell * )`: install the cell — nothing else; the cell-info
    /// tree is the manager's, delivered by [`Space::update_geometry`].
    pub fn set_cell(&mut self, cell: Rc<RefCell<Cell>>) {
        self.cell = Some(cell);
    }

    /// `cellInfos_` gaining another cell: a multi-cell space never
    /// self-reclaims.
    pub fn add_cell(&mut self, info: CellInfo) {
        self.cells.push(info);
    }

    /// `hasSingleCell`: `pCell_ && ((SpaceNode*)&pCell_->cellInfo() ==
    /// pCellInfoTree_)` — the space's one cell is the tree root.
    pub fn has_single_cell(&self) -> bool {
        match &self.cell {
            Some(cell) => self.cells.len() == 1 && self.cells[0].node == cell.borrow().info.node,
            None => false,
        }
    }

    /// `isShuttingDown`: the timer has been armed and not cancelled.
    pub fn is_shutting_down(&self) -> bool {
        self.timer_armed_at.is_some()
    }

    /// `cellInfos_`: every cell this space spans, tree order.
    pub fn cells(&self) -> impl Iterator<Item = &CellInfo> + '_ {
        self.cells.iter()
    }

    /// `spaceEntities()` (`entities_`): every entity the space holds,
    /// reals and ghosts alike.
    pub fn space_entities(&self) -> &[Rc<RefCell<Entity>>] {
        &self.entities
    }

    /// `addEntity( Entity * )`.
    pub fn add_entity(&mut self, entity: Rc<RefCell<Entity>>) {
        assert_eq!(
            entity.borrow().removal_handle,
            NO_SPACE_REMOVAL_HANDLE,
            "addEntity on an entity a space still holds"
        );
        let handle = self.entities.len();
        let mut borrowed = entity.borrow_mut();
        borrowed.removal_handle = handle;
        borrowed.space_id = self.id;
        drop(borrowed);
        self.entities.push(entity);
    }

    /// `removeEntity( Entity * )`: the swap-with-back — the back element's
    /// removal handle moves onto the vacated slot, the entity's own handle
    /// resets, and only then does the shutdown check run (the reference's
    /// comment: the removal can set off triggers that add or remove).
    pub fn remove_entity(&mut self, entity: &Rc<RefCell<Entity>>, cell_app: &mut CellApp) {
        let handle = entity.borrow().removal_handle;
        assert!(
            handle < self.entities.len() && self.entities[handle].borrow().id == entity.borrow().id,
            "removal handle does not name this entity"
        );

        let back = self.entities.last().cloned().expect("handle in range");
        back.borrow_mut().removal_handle = handle;
        self.entities[handle] = back;
        entity.borrow_mut().removal_handle = NO_SPACE_REMOVAL_HANDLE;
        self.entities.pop();

        // `pEntity->removeFromRangeList( rangeList_, appealRadiusList_ )`
        // — after the bookkeeping (the reference's comment: the removal
        // can set off triggers that call back into add/removeEntity), the
        // entity's own triggers go first (each remove fires the leaves
        // inside its still-live bounds), then the entity node exits
        // through `f32::MAX` (firing the leaves of every trigger that
        // watched it). The appeal-radius face is the AoI lane's.
        let (range_node, range_triggers) = {
            let borrowed = entity.borrow();
            (borrowed.range_node, borrowed.range_triggers.clone())
        };
        if let (Some(node), Some(list)) = (range_node, cell_app.range_lists.get_mut(&self.id)) {
            for &trigger in &range_triggers {
                list.trigger_remove(trigger);
            }
            list.entity_remove(node);
            let mut borrowed = entity.borrow_mut();
            borrowed.range_node = None;
            borrowed.range_triggers.clear();
        }

        self.check_for_shut_down(cell_app);
    }

    /// `checkForShutDown`: an emptied single-cell space asks for its
    /// shutdown, with the reference's own refusals — a multi-cell space, a
    /// space already shutting down, a CellApp that has not started, a
    /// dispatcher mid break, and the default space.
    pub fn check_for_shut_down(&mut self, cell_app: &mut CellApp) -> bool {
        let eligible = self.has_single_cell()
            && self.entities.is_empty()
            && cell_app.has_started()
            && !self.is_shutting_down()
            && !cell_app.processing_broken()
            && !(cell_app.use_default_space && self.id == 1);
        if eligible {
            self.request_shut_down(cell_app);
        }
        eligible
    }

    /// `requestShutDown`: ask the CellAppMgr, which routes the message
    /// back to the hosting CellApp. The reference still sends for the
    /// default space (it errors about it first).
    pub fn request_shut_down(&mut self, cell_app: &mut CellApp) -> SpaceId {
        let id = self.id;
        cell_app.shut_down_space(id);
        id
    }

    /// `shutDownSpace( BinaryIStream & )` arriving at `now_us`: the
    /// CellAppMgr's message, arming the recurring timer once. A second
    /// message finds it already set and does nothing.
    pub fn shut_down_space(&mut self, now_us: u64) -> bool {
        if self.timer_armed_at.is_none() {
            self.timer_armed_at = Some(now_us);
            true
        } else {
            false
        }
    }

    /// Advance the space's timer clock to `now_us`, firing the armed
    /// `ShutdownSpace` timer once for each delay width that has elapsed —
    /// the reference's `handleTimeout` on its recurring timer.
    pub fn tick(&mut self, now_us: u64, cell_app: &mut CellApp) -> bool {
        let mut fired = false;
        while let Some(armed_at) = self.timer_armed_at {
            if now_us < armed_at + SHUTDOWN_SPACE_DELAY_US {
                break;
            }
            self.timer_armed_at = Some(armed_at + SHUTDOWN_SPACE_DELAY_US);
            self.handle_timeout(cell_app);
            fired = true;
        }
        fired
    }

    /// `handleTimeout`: tell the cell the space is gone, and destroy the
    /// cell when this is a single-cell space with nothing left in it.
    /// Otherwise — multi-cell space, entities back, or cell already gone —
    /// the recurring timer simply re-checks on its next width ("deletion
    /// will occur once all chunks have been unloaded").
    fn handle_timeout(&mut self, cell_app: &mut CellApp) {
        let cell = match self.cell.clone() {
            Some(cell) => cell,
            None => return,
        };
        cell.borrow_mut().space_gone_count += 1;
        if self.has_single_cell() && self.entities.is_empty() {
            let node = cell.borrow().info.node;
            cell_app.destroy_cell(&cell);
            self.cell = None;
            self.cells.retain(|info| info.node != node);
        }
    }

    /// The `~Space` face: a live ghost is destroyed, a live real entity is
    /// warned about and kept, and the shutdown timer is cancelled.
    pub fn reclaim_entities(&mut self) -> EntityReclaim {
        let mut reclaimed = EntityReclaim::default();
        let mut kept = Vec::with_capacity(self.entities.len());
        for entity in self.entities.drain(..) {
            let (is_real, destroyed, id) = {
                let borrowed = entity.borrow();
                (borrowed.is_real, borrowed.destroyed, borrowed.id)
            };
            if !is_real && !destroyed {
                entity.borrow_mut().destroyed = true;
                reclaimed.destroyed_ghosts.push(id);
            } else if is_real && !destroyed {
                reclaimed.warning_real.push(id);
                kept.push(entity);
            }
        }
        self.entities = kept;
        self.timer_armed_at = None;
        reclaimed
    }

    /// The full `~Space` face (`space.cpp:95-133`): the ghosts go, the
    /// shutdown timer is cancelled, the tree is deleted, and — line 132 —
    /// the physical space is `clear()`ed before the space releases its
    /// reference to it. That release is what drops the reference count of
    /// the sole holder to zero and destroys the backend.
    pub fn destroy(&mut self) -> EntityReclaim {
        let reclaimed = self.reclaim_entities();
        if let Some(physical) = self.physical.take() {
            physical.borrow_mut().clear();
        }
        reclaimed
    }
}

/// `Spaces::deleteOldSpaces` (`spaces.cpp:86-105`), the game tick's reaper:
/// a space without a cell that has fully unloaded is erased from the
/// container and destroyed — the only place the container lets a `Space`
/// die. Returns the spaces it deleted.
pub fn delete_old_spaces(spaces: &mut BTreeMap<SpaceId, Space>) -> Vec<SpaceId> {
    let gone: Vec<SpaceId> = spaces
        .iter()
        .filter(|(_, space)| space.cell.is_none() && space.is_fully_unloaded())
        .map(|(id, _)| *id)
        .collect();
    for id in &gone {
        spaces.remove(id).expect("just measured").destroy();
    }
    gone
}

/// What [`Space::reclaim_entities`] did — the `~Space` bookkeeping made
/// observable.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EntityReclaim {
    /// The ghosts the space destroyed.
    pub destroyed_ghosts: Vec<u32>,
    /// The live real entities it warned about instead of destroying.
    pub warning_real: Vec<u32>,
}

/// The `CellApp::addCell` message's payload, decoded from the manager.
#[derive(Clone, Debug)]
pub struct AddCellRequest {
    /// The space the manager is adding a cell of.
    pub space_id: SpaceId,
    /// The `updateGeometry` payload: the space's tree cells.
    pub cells: Vec<CellInfo>,
    /// `isFirstCell`.
    pub is_first_cell: bool,
    /// `isFromDB`.
    pub is_from_db: bool,
    /// The `allSpaceData` payload: `(entryID, key, value)` entries. Empty
    /// means the message carried no space data at all
    /// (`remainingLength() == 0`).
    pub space_data: Vec<(u32, u16, Vec<u8>)>,
}

/// What `CellApp::addCell` did — the reference's branch matrix made
/// observable.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AddCellOutcome {
    /// An existing space was found and re-used ("Re-using space %d").
    pub space_reused: bool,
    /// An undestroyed cell was re-used ("Cell did not fully remove;
    /// reusing.").
    pub cell_reused: bool,
    /// A new cell was built and added to `cells_`.
    pub cell_registered: bool,
    /// `onGetFirstCell( isFromDB )` fired — the first cell of the default
    /// space.
    pub first_cell_delivered: bool,
    /// The CRITICAL path: no cell of this space belongs to this CellApp.
    pub no_cell_here: bool,
}

/// What [`Space::update_geometry`] reported — the reference's
/// `updateGeometry` bookkeeping made observable.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TopologyReport {
    /// The cells the new tree stopped carrying.
    pub deleted: Vec<CellInfoNode>,
    /// This CellApp's own cell was among them.
    pub own_cell_gone: bool,
    /// `pCell_->checkOffloadsAndGhosts()` ran (the own cell stays).
    pub cell_checked_off: bool,
    /// `wasMulticell` held, so the space re-checked the shutdown.
    pub checked_for_shut_down: bool,
    /// That re-check asked for the shutdown.
    pub requested_shut_down: bool,
}

/// `~Space`'s tail as a safety net: a `Space` that dies without going
/// through [`Space::destroy`] still clears its backend before the
/// reference count drops — the reference's line 132, which is what lets
/// `~EdgeGeometryMappings` assert the mappings were drained.
impl Drop for Space {
    fn drop(&mut self) {
        if let Some(physical) = self.physical.take() {
            physical.borrow_mut().clear();
        }
    }
}

/// The manager's own ledger of what it created and assigned — the side
/// [`CellApp::add_cell`]'s delivery must agree with.
#[derive(Clone, Debug, Default)]
pub struct MgrLedger {
    /// The spaces the manager created.
    pub spaces: Vec<SpaceId>,
    /// The cells it assigned: `(space, CellApp address, tree node)`.
    pub assignments: Vec<(SpaceId, u32, CellInfoNode)>,
}

impl MgrLedger {
    /// The manager and this CellApp agree about what they hold each other
    /// to: every space the ledger created exists here, every assignment
    /// addressed to this CellApp is registered here or already destroyed,
    /// and this CellApp holds no cell the ledger never assigned to it.
    pub fn agrees_with(&self, app: &CellApp, spaces: &BTreeMap<SpaceId, Space>) -> bool {
        if !self.spaces.iter().all(|id| spaces.contains_key(id)) {
            return false;
        }
        for (id, addr, node) in &self.assignments {
            if *addr != app.address {
                continue;
            }
            let registered = spaces
                .get(id)
                .and_then(|space| space.cell().cloned())
                .map(|cell| cell.borrow().info.node == *node)
                .unwrap_or(false);
            if !registered && !app.destroyed_cell_nodes.contains(node) {
                return false;
            }
        }
        app.cells.iter().all(|cell| {
            let borrowed = cell.borrow();
            self.assignments.iter().any(|(space_id, addr, node)| {
                *addr == app.address
                    && *node == borrowed.info.node
                    && *space_id == borrowed.info.space_id
            })
        })
    }
}

/// The CellApp state the shutdown decision reads (`CellApp::instance()` +
/// its dispatcher + config), recording what the Space said to it.
#[derive(Debug)]
pub struct CellApp {
    /// `CellApp::instance().hasStarted()`.
    started: bool,
    /// `CellAppConfig::useDefaultSpace()`.
    use_default_space: bool,
    /// `mainDispatcher().processingBroken()`.
    broken: bool,
    /// `interface_.address()`: who this CellApp is on the internal network.
    pub address: u32,
    /// `cells_`: every cell this CellApp holds, in registration order.
    pub cells: Vec<Rc<RefCell<Cell>>>,
    /// The `shutDownSpace` requests the spaces sent (the CellAppMgr half
    /// of the round trip).
    pub shut_down_requests: Vec<SpaceId>,
    /// The `destroyCell` calls, by cell's tree identity.
    pub destroyed_cell_nodes: Vec<CellInfoNode>,
    /// The `onGetFirstCell( isFromDB )` deliveries.
    pub first_cell_deliveries: Vec<bool>,
    /// The CRITICAL `addCell` deliveries: no cell of that space is ours.
    pub failed_adds: Vec<SpaceId>,
    /// The `Spaces::create` calls.
    pub created_spaces: Vec<SpaceId>,
    /// The `updateSpaceData` messages the spaces sent to the CellAppMgr.
    pub space_data_updates: Vec<(SpaceId, u32, u16)>,
    /// The `spaceDataEntry` refusals: a re-add of a recently deleted entry.
    pub rejected_readds: Vec<u32>,
    /// `IGameDelegate::instance() != NULL`: which physical-space backend a
    /// created space mounts.
    pub use_game_delegate: bool,
    /// The game-delegate world factory the delegate backend calls into.
    pub world_factory: Rc<RefCell<SpaceWorldFactory>>,
    /// `Entity::population()`: every entity id this process knows, including
    /// destroyed entries awaiting revival.
    pub entity_population: BTreeMap<u32, Rc<RefCell<Entity>>>,
    /// The entity-type script registry (`EntityType::getType`): which types
    /// can be on a cell and what their scripts define.
    pub entity_scripts: BTreeMap<u32, crate::cellapp_real_entity::EntityScript>,
    /// `Entity::s_callbackBuffer_`: the buffered script callbacks.
    pub callback_buffer: crate::cellapp_real_entity::CallbackBuffer,
    /// The script calls, in the order the reference would have made them.
    pub script_calls: Vec<crate::cellapp_real_entity::ScriptCall>,
    /// `Entity::callback` failures where the method was expected
    /// (`okIfFnNull` false).
    pub missing_callbacks: Vec<(u32, &'static str)>,
    /// The ERROR `to a destroyed entity` deliveries.
    pub destroyed_callbacks: Vec<(u32, &'static str)>,
    /// The WARNING `%s %u already exists!` creations.
    pub duplicate_creates: Vec<u32>,
    /// `Cell::addRealEntity`'s delayed `currentCell` sends.
    pub current_cell_sends: Vec<(u32, SpaceId)>,
    /// `time_`: the game time in ticks.
    pub time: u32,
    /// `ServerAppConfig::updateHertz()` (default 10).
    pub update_hertz: u32,
    /// The game-tick `TimeQueue` the controllers add to.
    pub time_queue: crate::timers::TimeQueue,
    /// The router events the queue produced on the last process.
    pub router_events: Rc<RefCell<Vec<crate::cellapp_real_entity::RouterEvent>>>,
    /// The `IDClient` pool entity ids are allocated from.
    pub id_pool: crate::entity_creator::IdPool,
    /// `CellAppConfig::expectedMaxControllers` (25).
    pub expected_max_controllers: usize,
    /// `CellAppConfig::absoluteMaxControllers` (50).
    pub absolute_max_controllers: usize,
    /// `Controller::s_exclusiveIDs`: the exclusive categories seen so far
    /// (`getExclusiveID`); their ids sit above the mask.
    pub exclusive_ids: BTreeMap<&'static str, u16>,
    /// `Entity::s_entityExtraInfo`: the entity-extra type registry; the
    /// index a type registers at is its eeid.
    pub extra_types: Vec<&'static str>,
    /// The `EntityExtra` destruction ledger: `(entity, eeid, generation)`
    /// per deleted instance — the observable face of the reference's
    /// `delete pExtra`.
    pub extra_drops: Rc<RefCell<Vec<(u32, usize, u64)>>>,
    /// Which instance a slot's next touch constructs.
    pub next_extra_generation: u64,
    /// The `Updatables` registry (`CellApp::registerForUpdate`): the
    /// movement-family controllers that drive every tick, in
    /// registration order. A slot vacated mid-pass stays `None` until the
    /// pass ends (the reference's in-update hole), so the pass iterates
    /// the size it started with.
    pub updatable_controllers: Vec<Option<(u32, crate::cellapp_real_entity::ControllerId)>>,
    /// `Updatables::inUpdate_`: whether a pass is running; removals
    /// during it punch holes instead of swapping.
    pub in_movement_update: bool,
    /// The deterministic replacement of the reference's libc `rand()`
    /// (a xorshift64* stream): `MoveToEntityController::recalcOffset`
    /// draws its angle from it. The reference seeds libc `rand()` from
    /// the clock in `main`; the port fixes the stream for reproducible
    /// trajectories.
    pub movement_rng: u64,
    /// The `PyErr_SetString` faces of the controller script factories
    /// (e.g. `YawRotatorController::New`'s velocity check): the rejected
    /// `(entity, message)` ledger.
    pub script_errors: Vec<(u32, &'static str)>,
    /// `CellAppConfig::shouldNavigationDropPosition` (default false).
    pub should_navigation_drop_position: bool,
    /// `CellAppConfig::navigationMaxClimb` (0.65).
    pub navigation_max_climb: f32,
    /// `CellAppConfig::navigationMaxSlopeGradient`
    /// (`tan(deg2rad(navigationMaxSlope))`, 45° default → 1.0).
    pub navigation_max_slope_gradient: f32,
    /// `CellAppConfig::maxAoIRadius` (`DEFAULT_AOI_RADIUS`, 500) — the
    /// `-1` max-search-distance substitution the navigation script
    /// factories make.
    pub max_aoi_radius: f32,
    /// `EntityNavigate::pNavigateStepController_`, by entity: the
    /// navigate-step controller currently registered on the extra.
    pub navigate_step_controllers: BTreeMap<u32, crate::cellapp_real_entity::ControllerId>,
    /// `NavmeshEntityNavigationSystem`'s query stack, by entity (`Entity`
    /// owns its `EntityNavigate` extra, which owns the system). An
    /// absent entry is the reference's `!pChunkSpace()` face: navigation
    /// queries are unsupported there.
    pub navigators: BTreeMap<u32, crate::cellapp_navigate::NavigatorHandle>,
    /// The navigation-family WARNINGs: the unsupported-space `startReal`
    /// refusal, the `closeEnough` ctor complaint and the
    /// NavigateStepController 100-loop bail.
    pub navigation_warnings: Vec<(u32, String)>,
    /// Per-vehicle `Passengers` extra state (BW-08-02-05): the rider
    /// list and the global-to-child transform.
    pub passengers_state: BTreeMap<u32, crate::cellapp_passenger::PassengersState>,
    /// `PassengerExtra::pController_` by passenger: the controller id
    /// riding the extra (`None`-as-absent — a cleared extra leaves no
    /// entry).
    pub passenger_controllers: BTreeMap<u32, crate::cellapp_real_entity::ControllerId>,
    /// The passenger-family WARNING/ERROR/INFO/DEBUG/CRITICAL texts.
    pub passenger_warnings: Vec<(u32, String)>,
    /// `EntityPopulation` observers (`entity_population.cpp`): the ids
    /// waiting on a vehicle id to enter the population (the limbo face).
    pub population_observers: BTreeMap<u32, Vec<u32>>,
    /// `RealEntity::sendPhysicsCorrection`: the entity ids a correction
    /// went out for (board/alight on a client-controlled entity).
    pub physics_corrections: Vec<u32>,
    /// `BaseAppIntInterface::cellEntityLost` sends recorded by destroy
    /// (`sendCellEntityLostToBase`): the base entities told that their
    /// cell half is gone.
    pub cell_entity_lost_sends: Vec<u32>,
    /// `idClient().putUsedID` calls recorded by destroy
    /// (`~Entity`'s `shouldReturnID_` face): the cell-only entity ids
    /// handed back — a recorded no-op while `MF_ID_RECYCLING` is off.
    pub returned_entity_ids: Vec<u32>,
    /// Each space's `rangeList_` (`space.hpp:212`): the arena of nodes
    /// every entity and trigger bound of the space lives on. Held on the
    /// CellApp rather than the `Space` itself — this port's spaces are
    /// deliberately external (the position-write funnel cannot reach
    /// per-space data), the passengers' state is the standing precedent.
    pub range_lists: BTreeMap<SpaceId, crate::cellapp_range_list::RangeList>,
    /// The witnesses, by the entity whose client they serve (the
    /// reference keeps one `Witness*` per real entity with a witness —
    /// `Entity::pWitness()`), BW-08-03.
    pub witnesses: BTreeMap<u32, crate::cellapp_witness::Witness>,
    /// `CellAppConfig::aoiUpdateSchemes()`.
    pub aoi_schemes: crate::cellapp_witness::AoIUpdateSchemes,
    /// The per-type description faces the witness reads
    /// (`EntityDescription::canBeOnClient` and friends).
    pub entity_type_faces: BTreeMap<u32, crate::cellapp_witness::EntityTypeDef>,
    /// `Entity::propertyEventStamps()`, per entity: the client-server
    /// properties and their change stamps.
    pub entity_properties: BTreeMap<u32, Vec<crate::cellapp_witness::ClientProperty>>,
    /// The witness WARNING/INFO faces, in delivery order.
    pub witness_warnings: Vec<crate::cellapp_witness::WitnessNote>,
    /// The witness ERROR/CRITICAL faces, in delivery order.
    pub witness_errors: Vec<crate::cellapp_witness::WitnessNote>,
    /// `CellApp::emergencyThrottle()`: the load throttle (1.0 is none).
    pub emergency_throttle: f32,
    /// `CellAppConfig::witnessUpdateMaxPriorityDelta()` (default 5.0).
    pub witness_max_priority_delta: f32,
    /// The fixed per-send bundle growth while the update writer is
    /// BW-08-03-04's face.
    /// `CellAppConfig::entitySpamSize()` (default 200).
    pub witness_spam_size: i32,
    /// `Space::s_allSpacesDataChangeSeq_`: the global space-data
    /// sequence the witnesses compare against.
    pub all_spaces_data_change_seq: u64,
    /// The spaces, by id. Held on the CellApp like the range lists —
    /// this port's spaces are deliberately external, and the witness's
    /// space-data face reaches for the witnessing entity's own space
    /// through here.
    pub spaces: BTreeMap<SpaceId, Space>,
    /// `CellAppConfig::maxTimeBetweenPlayerUpdatesInTicks()`: how long a
    /// player's detailed position may ride unchanged. `-1` always skips
    /// the repeat, `0` never does.
    pub max_time_between_player_updates: i32,
    /// `CellAppConfig::packedXZScale()`: `maxAoIRadius` over the floor of
    /// the packed limit.
    pub packed_xz_scale: f32,
    /// `CellAppConfig::sendDetailedPlayerVehicles()` (default false).
    pub send_detailed_player_vehicles: bool,
    /// The `onWitnessed` callback deliveries the send pass made.
    pub witnessed_notifications: Vec<(u32, bool)>,
    /// `CellAppConfig::ghostDistance()` (the `DEFAULT_AOI_RADIUS`, 500).
    pub ghost_distance: f32,
    /// `CellAppConfig::maxGhostsToDelete()` (default 100).
    pub max_ghosts_to_delete: u32,
    /// The ghost wires the maintainer sent: `createGhost`/`delGhost`
    /// bundles, in send order, per destination address. The tests deliver
    /// them into the peer CellApp; the reference hands them to
    /// `CellAppChannels`' bundles.
    pub ghost_outbox: Vec<crate::cellapp_ghost::GhostWire>,
    /// The `CellAppInterface::onload` wires an offload streamed: the
    /// real entity's data bound for the app it is moving to. The tests
    /// deliver them into the peer CellApp; the reference hands them to
    /// the destination's bundle.
    pub onload_outbox: Vec<crate::cellapp_ghost::OnloadWire>,
    /// `CellAppInterface::onloadTeleportedEntity` wires: the cross-space
    /// teleport's head (the nearby id and the ghost body) with the tail
    /// `onload` boxed inside.
    pub teleport_outbox: Vec<crate::cellapp_ghost::TeleportWire>,
    /// `BaseAppIntInterface::emergencySetCurrentCell` wires from the
    /// death sweep: one per zombied ghost with a base.
    pub emergency_outbox: Vec<crate::cellapp_ghost::EmergencySetCurrentCellWire>,
    /// `CellAppChannels::hasRecentlyDied`: apps the death sweep marked
    /// gone, so a ghost whose next real names one still zombies.
    pub dead_apps: BTreeSet<u32>,
    /// `CellAppChannels`' per-channel `mark()` — the ghost maintainer's
    /// this-round mark bookkeeping, by remote address.
    pub channel_marks: BTreeMap<u32, i32>,
    /// `CellAppChannel::isGood()`'s failed set: the addresses a
    /// `channel failure` took down. Empty by default.
    pub dead_channels: BTreeSet<u32>,
    /// `pEntityType_->description().property( index )`: the port's
    /// property-description table (the reference's def-driven one), keyed
    /// by entity type. Each entry is the (property index, componentised
    /// flag) pair `ghostedDataUpdate` looks up.
    pub ghost_property_descriptions: BTreeMap<u32, Vec<(i32, bool /* componentised */)>>,
    /// The reference's `countReceived` / CRITICAL / ERROR notes, kept as
    /// text in the port.
    pub ghost_property_notes: Vec<String>,
    /// `BufferedGhostMessages`: the switch-period messages held per
    /// ghost, keyed by entity id.
    pub buffered_ghost_messages:
        BTreeMap<u32, crate::cellapp_ghost::BufferedGhostMessagesForEntity>,
    /// `EntityPopulation::currChannels_`: the real channel address
    /// cached for an entity id this app has seen as a ghost.
    pub curr_real_channels: BTreeMap<u32, u32>,
    /// `EntityPopulation::prevChannels_`: the previous generation of
    /// cached real channels, still good until the trim rolls it over.
    pub prev_real_channels: BTreeMap<u32, u32>,
    /// `Entity::forwardMessageToReal` bundles: real-domain calls
    /// forwarded to the cached real, with their original reply id.
    pub real_call_outbox: Vec<crate::cellapp_ghost::RealCallWire>,
}

impl Default for CellApp {
    /// A fresh CellApp: not started, nothing broken, chunk backend.
    fn default() -> Self {
        Self {
            started: false,
            use_default_space: false,
            broken: false,
            address: 0,
            cells: Vec::new(),
            shut_down_requests: Vec::new(),
            destroyed_cell_nodes: Vec::new(),
            first_cell_deliveries: Vec::new(),
            failed_adds: Vec::new(),
            created_spaces: Vec::new(),
            space_data_updates: Vec::new(),
            rejected_readds: Vec::new(),
            use_game_delegate: false,
            world_factory: Rc::new(RefCell::new(SpaceWorldFactory::default())),
            entity_population: BTreeMap::new(),
            entity_scripts: BTreeMap::new(),
            callback_buffer: crate::cellapp_real_entity::CallbackBuffer::default(),
            script_calls: Vec::new(),
            missing_callbacks: Vec::new(),
            destroyed_callbacks: Vec::new(),
            duplicate_creates: Vec::new(),
            current_cell_sends: Vec::new(),
            time: 0,
            update_hertz: 10,
            time_queue: crate::timers::TimeQueue::new(),
            router_events: Rc::new(RefCell::new(Vec::new())),
            id_pool: crate::entity_creator::IdPool::new(10, 30, 100, 130),
            expected_max_controllers: 25,
            absolute_max_controllers: 50,
            exclusive_ids: BTreeMap::new(),
            extra_types: Vec::new(),
            extra_drops: Rc::new(RefCell::new(Vec::new())),
            next_extra_generation: 0,
            updatable_controllers: Vec::new(),
            in_movement_update: false,
            movement_rng: 0x9E37_79B9_7F4A_7C15,
            script_errors: Vec::new(),
            cell_entity_lost_sends: Vec::new(),
            returned_entity_ids: Vec::new(),
            range_lists: BTreeMap::new(),
            should_navigation_drop_position: false,
            navigation_max_climb: 0.65,
            navigation_max_slope_gradient: 1.0,
            max_aoi_radius: 500.0,
            navigate_step_controllers: BTreeMap::new(),
            navigators: BTreeMap::new(),
            navigation_warnings: Vec::new(),
            passengers_state: BTreeMap::new(),
            passenger_controllers: BTreeMap::new(),
            passenger_warnings: Vec::new(),
            population_observers: BTreeMap::new(),
            physics_corrections: Vec::new(),
            witnesses: BTreeMap::new(),
            aoi_schemes: crate::cellapp_witness::AoIUpdateSchemes::default(),
            entity_type_faces: BTreeMap::new(),
            entity_properties: BTreeMap::new(),
            witness_warnings: Vec::new(),
            witness_errors: Vec::new(),
            emergency_throttle: 1.0,
            witness_max_priority_delta: 5.0,
            witness_spam_size: 200,
            all_spaces_data_change_seq: 0,
            spaces: BTreeMap::new(),
            packed_xz_scale: 500.0 / 28.0,
            send_detailed_player_vehicles: false,
            witnessed_notifications: Vec::new(),
            ghost_distance: crate::cellapp_ghost::DEFAULT_AOI_RADIUS,
            max_ghosts_to_delete: crate::cellapp_ghost::MAX_GHOSTS_TO_DELETE,
            ghost_outbox: Vec::new(),
            onload_outbox: Vec::new(),
            teleport_outbox: Vec::new(),
            emergency_outbox: Vec::new(),
            dead_apps: BTreeSet::new(),
            channel_marks: BTreeMap::new(),
            dead_channels: BTreeSet::new(),
            ghost_property_descriptions: BTreeMap::new(),
            ghost_property_notes: Vec::new(),
            buffered_ghost_messages: BTreeMap::new(),
            curr_real_channels: BTreeMap::new(),
            prev_real_channels: BTreeMap::new(),
            real_call_outbox: Vec::new(),
            max_time_between_player_updates: 0,
        }
    }
}

impl CellApp {
    /// A fresh CellApp: not started, nothing broken.
    pub fn new() -> Self {
        Self::default()
    }

    /// `CellApp::instance().hasStarted()` turning true.
    pub fn start(&mut self) {
        self.started = true;
    }

    /// `hasStarted`.
    pub fn has_started(&self) -> bool {
        self.started
    }

    /// `CellAppConfig::useDefaultSpace`.
    pub fn set_use_default_space(&mut self, on: bool) {
        self.use_default_space = on;
    }

    pub fn use_default_space(&self) -> bool {
        self.use_default_space
    }

    /// `mainDispatcher().processingBroken`.
    pub fn set_processing_broken(&mut self, broken: bool) {
        self.broken = broken;
    }

    pub fn processing_broken(&self) -> bool {
        self.broken
    }

    /// `CellAppMgr::shutDownSpace` receiving the Space's request.
    fn shut_down_space(&mut self, space_id: SpaceId) {
        self.shut_down_requests.push(space_id);
    }

    /// A fresh CellApp at `address`: not started, nothing broken, no cells.
    pub fn with_address(address: u32) -> Self {
        Self {
            address,
            ..Self::default()
        }
    }

    /// `CellApp::destroyCell`: the cell goes away; the space clears its
    /// own `pCell_` (the reference's destructed cell does it) and the
    /// `cells_` registry forgets it.
    pub fn destroy_cell(&mut self, cell: &Rc<RefCell<Cell>>) {
        let node = cell.borrow().info.node;
        self.destroyed_cell_nodes.push(node);
        self.cells.retain(|held| held.borrow().info.node != node);
    }

    /// `CellApp::findSpace`.
    pub fn find_space<'a>(
        &self,
        spaces: &'a BTreeMap<SpaceId, Space>,
        id: SpaceId,
    ) -> Option<&'a Space> {
        spaces.get(&id)
    }

    /// `CellApp::findCell( SpaceID )`: the space's own cell.
    pub fn find_cell(
        &self,
        spaces: &BTreeMap<SpaceId, Space>,
        id: SpaceId,
    ) -> Option<Rc<RefCell<Cell>>> {
        spaces.get(&id).and_then(|space| space.cell().cloned())
    }

    /// `CellApp::addCell`: the manager's message adding a cell of
    /// `space_id` to this CellApp — find or create the space, re-use it,
    /// read the tree, then find, re-use or build the cell that is ours,
    /// and finally carry the space data and the first-cell event.
    pub fn add_cell(
        &mut self,
        spaces: &mut BTreeMap<SpaceId, Space>,
        request: AddCellRequest,
    ) -> AddCellOutcome {
        let mut outcome = AddCellOutcome::default();
        let space = match spaces.get_mut(&request.space_id) {
            Some(space) => {
                outcome.space_reused = true;
                space.reuse();
                space
            }
            None => {
                self.created_spaces.push(request.space_id);
                // `Spaces::create` → `new Space( id )`, mounting whichever
                // backend the game-delegate flag picks.
                let delegate_mode = self.use_game_delegate;
                let factory = Rc::clone(&self.world_factory);
                spaces.entry(request.space_id).or_insert_with(|| {
                    if delegate_mode {
                        Space::new_delegate(request.space_id, factory)
                    } else {
                        Space::new(request.space_id)
                    }
                })
            }
        };

        // `pSpace->updateGeometry(data)`: the marks, the rebuilt tree and the
        // re-check of the shutdown are all part of that call.
        space.update_geometry(self, request.cells);
        let info = match space.find_cell_info(self.address) {
            Some(info) => info,
            None => {
                outcome.no_cell_here = true;
                self.failed_adds.push(request.space_id);
                return outcome;
            }
        };

        let existing = space.cell().cloned();
        match existing {
            Some(cell) => {
                assert_eq!(
                    info.node,
                    cell.borrow().info.node,
                    "the re-added cell is not the one that did not fully remove"
                );
                cell.borrow_mut().reuse();
                outcome.cell_reused = true;
            }
            None => {
                let cell = Rc::new(RefCell::new(Cell {
                    info: info.clone(),
                    space_gone_count: 0,
                    reuse_count: 0,
                    should_offload: true,
                    is_removed: false,
                }));
                space.set_cell(cell.clone());
                self.cells.push(cell);
                outcome.cell_registered = true;
            }
        }

        space.read_data_from_stream(self, request.space_data);

        // The first cell in the first space.
        if request.is_first_cell && self.use_default_space && request.space_id == 1 {
            self.first_cell_deliveries.push(request.is_from_db);
            outcome.first_cell_delivered = true;
        }
        outcome
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The test CellApp's own address on the internal network.
    const APP_ADDR: u32 = 10;

    fn entity(id: u32, real: bool) -> Rc<RefCell<Entity>> {
        Rc::new(RefCell::new(Entity::new(id, real)))
    }

    fn cell_info(space_id: SpaceId, node: usize) -> CellInfo {
        CellInfo {
            space_id,
            app_addr: APP_ADDR,
            node: CellInfoNode(node),
            rect: (0.0, 0.0, 0.0, 0.0),
            is_delete_pending: false,
        }
    }

    /// A started CellApp, a single-cell space, and the cell it holds.
    fn fixture(id: SpaceId, node: usize) -> (CellApp, Space, Rc<RefCell<Cell>>) {
        let mut cell_app = CellApp::with_address(APP_ADDR);
        cell_app.start();
        let cell = Rc::new(RefCell::new(Cell {
            info: cell_info(id, node),
            space_gone_count: 0,
            reuse_count: 0,
            should_offload: true,
            is_removed: false,
        }));
        let mut space = Space::new(id);
        space.update_geometry(&mut cell_app, vec![cell_info(id, node)]);
        space.set_cell(cell.clone());
        (cell_app, space, cell)
    }

    /// A Space with no cell is never a single-cell space, and a timerless
    /// Space never ticks.
    #[test]
    fn a_cellless_space_never_reclaims() {
        let mut app = CellApp::new();
        app.start();
        let mut bare = Space::new(9);
        assert!(!bare.has_single_cell(), "no cell, no single cell");
        assert!(!bare.is_shutting_down());
        assert!(!bare.tick(10_000_000, &mut app), "no armed timer, no fire");
    }

    /// The acceptance face: an emptied single-cell Space asks for its
    /// shutdown, and one timer-width later the cell is gone — then the
    /// recurring timer keeps re-checking without destroying anything
    /// further.
    #[test]
    fn an_emptied_space_is_reclaimed() {
        let (mut app, mut space, cell) = fixture(7, 0);
        space.add_entity(entity(10, true));
        space.add_entity(entity(11, true));
        assert_eq!(space.num_entities(), 2);
        assert_eq!(space.num_cells(), 1);
        assert!(!space.is_shutting_down());

        // The first entity leaves; the space is not empty yet.
        space.remove_entity(&space.entities()[0].clone(), &mut app);
        assert!(!space.is_shutting_down());
        assert!(app.shut_down_requests.is_empty());

        // The last entity leaves: the space is now empty and asks the
        // manager for its shutdown — the request is not yet the timer, the
        // message still has to come back.
        let last = space.entities()[0].clone();
        space.remove_entity(&last, &mut app);
        assert!(!space.is_shutting_down(), "a request is not a timer");
        assert_eq!(app.shut_down_requests, vec![7u32]);

        // The manager's message comes back and arms the timer at t=0;
        // before the delay elapses nothing happens.
        assert!(space.shut_down_space(0));
        assert!(space.is_shutting_down());
        assert!(!space.tick(999_999, &mut app));
        assert!(space.cell().is_some());

        // At the delay the cell is told and destroyed, and the space
        // loses it.
        assert!(space.tick(1_000_000, &mut app));
        assert!(space.cell().is_none());
        assert_eq!(app.destroyed_cell_nodes, vec![CellInfoNode(0)]);
        assert_eq!(cell.borrow().space_gone_count, 1);
        assert_eq!(space.num_cells(), 0);
        assert!(space.is_shutting_down(), "a recurring timer stays armed");

        // Each further width re-checks and finds nothing left to do.
        assert!(!space.tick(1_500_000, &mut app));
        assert!(space.tick(2_000_000, &mut app));
        assert_eq!(cell.borrow().space_gone_count, 1, "no cell, no delivery");
        assert_eq!(
            app.destroyed_cell_nodes.len(),
            1,
            "nothing further destroyed"
        );
    }

    /// The removal handle is a swap-with-back: survivors keep the right
    /// index, the removed entity is out of every list, and the entity's
    /// own handle is reset.
    #[test]
    fn the_removal_handle_swaps_with_the_back() {
        let (mut app, mut space, _) = fixture(9, 0);
        let entities: Vec<_> = (1..=3).map(|id| entity(id, true)).collect();
        for entity in &entities {
            space.add_entity(entity.clone());
        }
        assert_eq!(
            entities
                .iter()
                .map(|e| e.borrow().removal_handle)
                .collect::<Vec<_>>(),
            vec![0usize, 1, 2]
        );

        // Removing the middle one moves the back element into its slot.
        space.remove_entity(&entities[1], &mut app);
        assert_eq!(entities[0].borrow().removal_handle, 0);
        assert_eq!(entities[1].borrow().removal_handle, NO_SPACE_REMOVAL_HANDLE);
        assert_eq!(entities[2].borrow().removal_handle, 1);
        let ids: Vec<_> = space.entities().iter().map(|e| e.borrow().id).collect();
        assert_eq!(ids, vec![1u32, 3]);
    }

    /// Every refusal the reference has: a multi-cell space, an unstarted
    /// CellApp, a broken dispatcher, and the default space. The direct
    /// `requestShutDown` still goes out for the default space — the
    /// reference errors about it and sends anyway.
    #[test]
    fn the_shutdown_gate_refuses_like_the_reference() {
        // A multi-cell space never self-reclaims, even emptied.
        let (mut app, mut multi, _) = fixture(2, 0);
        assert!(!app.use_default_space());
        multi.add_cell(CellInfo {
            space_id: 2,
            app_addr: APP_ADDR,
            node: CellInfoNode::next(CellInfoNode(0)),
            rect: (0.0, 0.0, 0.0, 0.0),
            is_delete_pending: false,
        });
        let e = entity(1, true);
        multi.add_entity(e.clone());
        multi.remove_entity(&e, &mut app);
        assert!(!multi.is_shutting_down());
        assert!(app.shut_down_requests.is_empty());

        // An unstarted CellApp holds every space back.
        let mut unstarted_app = CellApp::new();
        let mut not_started = Space::new(3);
        let cell = Rc::new(RefCell::new(Cell {
            info: cell_info(3, 1),
            space_gone_count: 0,
            reuse_count: 0,
            should_offload: true,
            is_removed: false,
        }));
        not_started.update_geometry(&mut unstarted_app, vec![cell_info(3, 1)]);
        not_started.set_cell(cell);
        let e = entity(2, true);
        not_started.add_entity(e.clone());
        not_started.remove_entity(&e, &mut unstarted_app);
        assert!(!not_started.is_shutting_down());
        assert!(unstarted_app.shut_down_requests.is_empty());

        // A dispatcher mid break does not reclaim either.
        let (mut broken_app, mut broken, _) = fixture(4, 2);
        broken_app.set_processing_broken(true);
        let e = entity(3, true);
        broken.add_entity(e.clone());
        broken.remove_entity(&e, &mut broken_app);
        assert!(!broken.is_shutting_down());
        assert!(broken_app.shut_down_requests.is_empty());

        // The default space refuses through the gate...
        let (mut default_app, mut default_space, _) = fixture(1, 3);
        default_app.set_use_default_space(true);
        assert!(default_app.use_default_space());
        let e = entity(4, true);
        default_space.add_entity(e.clone());
        assert!(
            !default_space.check_for_shut_down(&mut default_app),
            "the default space reclaimed"
        );
        assert!(default_app.shut_down_requests.is_empty());
        // ...but a direct requestShutDown still goes out.
        assert_eq!(default_space.request_shut_down(&mut default_app), 1);
        assert_eq!(default_app.shut_down_requests, vec![1u32]);
    }

    /// A second shutdown message finds the timer already armed and does
    /// not re-arm it: the delay stays anchored to the first arming.
    #[test]
    fn a_second_shutdown_message_does_not_re_arm() {
        let (mut app, mut space, _) = fixture(3, 0);
        let lonely = entity(8, true);
        space.add_entity(lonely.clone());
        space.remove_entity(&lonely, &mut app);
        assert_eq!(app.shut_down_requests, vec![3u32]);

        assert!(space.shut_down_space(500_000), "first message arms");
        assert!(!space.shut_down_space(600_000), "second finds it armed");
        assert!(!space.tick(1_499_999, &mut app));
        assert!(
            space.tick(1_500_000, &mut app),
            "fires one width after arming"
        );
        assert!(space.is_shutting_down(), "the recurring timer stays armed");
    }

    /// The timer on a multi-cell space delivers `onSpaceGone` every width
    /// but keeps the cell — deletion waits for the whole space.
    #[test]
    fn a_multi_cell_space_keeps_its_cell() {
        let (mut app, mut space, cell) = fixture(5, 0);
        space.add_cell(CellInfo {
            space_id: 5,
            app_addr: APP_ADDR,
            node: CellInfoNode::next(CellInfoNode(0)),
            rect: (0.0, 0.0, 0.0, 0.0),
            is_delete_pending: false,
        });
        assert!(space.shut_down_space(0));
        assert!(space.tick(1_000_000, &mut app));
        assert_eq!(cell.borrow().space_gone_count, 1);
        assert!(space.cell().is_some(), "multi-cell space keeps its cell");
        assert!(!space.has_single_cell(), "two cells left");

        // Entities arriving back hold the destruction off too.
        let (mut app2, mut repopulated, cell2) = fixture(6, 0);
        repopulated.shut_down_space(0);
        let back = entity(9, true);
        repopulated.add_entity(back.clone());
        assert!(repopulated.tick(1_000_000, &mut app2));
        assert_eq!(cell2.borrow().space_gone_count, 1);
        assert!(
            repopulated.cell().is_some(),
            "an occupied single-cell space keeps its cell"
        );
        assert_eq!(app2.destroyed_cell_nodes, Vec::<CellInfoNode>::new());
    }

    /// The `~Space` face: ghosts are destroyed, live real entities are
    /// warned about and kept, and the timer is cancelled.
    #[test]
    fn the_space_destructor_reclaims_its_ghosts() {
        let (mut app, mut space, _) = fixture(6, 0);
        let ghost = entity(100, false);
        let real = entity(101, true);
        // A ghost already destroyed is left where it is: nothing to do.
        let gone = entity(99, false);
        gone.borrow_mut().destroyed = true;
        space.add_entity(gone);
        space.add_entity(ghost.clone());
        space.add_entity(real.clone());
        space.shut_down_space(0);

        let reclaimed = space.reclaim_entities();
        assert_eq!(
            reclaimed,
            EntityReclaim {
                destroyed_ghosts: vec![100],
                warning_real: vec![101],
            }
        );
        assert!(ghost.borrow().destroyed);
        assert!(!real.borrow().destroyed);
        assert_eq!(space.num_entities(), 1);
        assert!(!space.is_shutting_down(), "destruction cancels the timer");
        assert!(
            !space.tick(10_000_000, &mut app),
            "a cancelled timer stays dead"
        );

        // An already-empty space reclaims nothing and still cancels.
        let (mut app2, mut empty, _) = fixture(7, 0);
        empty.shut_down_space(0);
        assert_eq!(empty.reclaim_entities(), EntityReclaim::default());
        assert!(!empty.is_shutting_down());
        assert!(
            !empty.tick(10_000_000, &mut app2),
            "and it never fires again"
        );
    }

    /// The acceptance face: the manager's `addCell` delivery leaves the
    /// manager and each CellApp agreeing — the space exists where the
    /// manager created it, and each assigned cell is registered on its own
    /// CellApp.
    #[test]
    fn add_cell_registers_the_cells_the_manager_assigned() {
        let mgr = MgrLedger {
            spaces: vec![5],
            assignments: vec![(5, 10, CellInfoNode(0)), (5, 11, CellInfoNode(1))],
        };
        let tree = vec![
            CellInfo {
                space_id: 5,
                app_addr: 10,
                node: CellInfoNode(0),
                rect: (0.0, 0.0, 0.0, 0.0),
                is_delete_pending: false,
            },
            CellInfo {
                space_id: 5,
                app_addr: 11,
                node: CellInfoNode(1),
                rect: (0.0, 0.0, 0.0, 0.0),
                is_delete_pending: false,
            },
        ];

        let mut spaces10 = BTreeMap::new();
        let mut app10 = CellApp::with_address(10);
        app10.start();
        let outcome = app10.add_cell(
            &mut spaces10,
            AddCellRequest {
                space_id: 5,
                cells: tree.clone(),
                is_first_cell: false,
                is_from_db: false,
                space_data: vec![(7u32, 300u16, vec![0u8]), (8, 2, vec![1])],
            },
        );
        assert_eq!(
            outcome,
            AddCellOutcome {
                cell_registered: true,
                ..Default::default()
            }
        );
        assert_eq!(app10.created_spaces, vec![5u32]);
        assert_eq!(app10.cells.len(), 1);
        assert_eq!(app10.cells[0].borrow().info.node, CellInfoNode(0));
        let space = app10.find_space(&spaces10, 5).expect("space");
        assert_eq!(space.num_cells(), 2);
        assert_eq!(space.space_data.size(), 2, "the payload was carried");
        assert_eq!(
            app10.find_cell(&spaces10, 5).map(|c| c.borrow().info.node),
            Some(CellInfoNode(0))
        );

        let mut spaces11 = BTreeMap::new();
        let mut app11 = CellApp::with_address(11);
        app11.start();
        let outcome = app11.add_cell(
            &mut spaces11,
            AddCellRequest {
                space_id: 5,
                cells: tree,
                is_first_cell: false,
                is_from_db: false,
                space_data: vec![(7, 300, vec![0]), (8, 2, vec![1])],
            },
        );
        assert!(outcome.cell_registered);
        assert_eq!(app11.cells[0].borrow().info.node, CellInfoNode(1));

        assert!(mgr.agrees_with(&app10, &spaces10));
        assert!(mgr.agrees_with(&app11, &spaces11));
    }

    /// The manager re-adding a cell it had added before re-uses the space
    /// and the cell: the data faces reset, the entities stay, and no
    /// duplicate registration appears.
    #[test]
    fn a_second_add_cell_reuses_the_space_and_the_cell() {
        let tree = vec![cell_info(5, 0)];
        let mut spaces = BTreeMap::new();
        let mut app = CellApp::with_address(APP_ADDR);
        app.start();
        let request = |space_data| AddCellRequest {
            space_id: 5,
            cells: tree.clone(),
            is_first_cell: false,
            is_from_db: false,
            space_data,
        };
        let first = app.add_cell(
            &mut spaces,
            request(vec![
                (1u32, 300u16, vec![0u8]),
                (2, 2, vec![1]),
                (3, 3, vec![2]),
            ]),
        );
        assert!(first.cell_registered && !first.space_reused);
        assert_eq!(
            spaces.get(&5).expect("space").space_data.size(),
            3,
            "every payload entry landed"
        );

        // Dirt the data faces and put an entity in, then let the manager
        // re-add the cell.
        {
            let space = spaces.get_mut(&5).expect("space");
            space.beg_data_seq = 7;
            space.end_data_seq = 9;
            space.initial_time_of_day = 0.5;
            space.game_seconds_per_second = 2.0;
            space.add_entity(entity(1, true));
        }
        let second = app.add_cell(&mut spaces, request(vec![]));
        assert_eq!(
            second,
            AddCellOutcome {
                space_reused: true,
                cell_reused: true,
                ..Default::default()
            }
        );
        let space = spaces.get(&5).expect("space");
        assert_eq!(space.beg_data_seq, 0, "reuse reset the data sequence");
        assert_eq!(space.end_data_seq, 0);
        assert_eq!(
            space.initial_time_of_day, -1.0,
            "reuse reset the time of day"
        );
        assert_eq!(space.game_seconds_per_second, 0.0);
        assert_eq!(
            space.space_data.size(),
            0,
            "an empty payload leaves the count at reuse's zero"
        );
        assert_eq!(space.num_entities(), 1, "reuse keeps the entities");
        assert_eq!(
            space.num_cells(),
            1,
            "the restored tree replaced the cleared one"
        );
        assert_eq!(app.cells.len(), 1, "no duplicate registration");
        assert_eq!(app.cells[0].borrow().reuse_count, 1);
        assert_eq!(app.created_spaces, vec![5u32], "the space was created once");
    }

    /// A space none of whose cells belong to this CellApp takes the
    /// CRITICAL path: the space is still created, but nothing registers.
    #[test]
    fn an_add_cell_without_our_cell_is_the_critical_path() {
        let mut spaces = BTreeMap::new();
        let mut app = CellApp::with_address(10);
        app.start();
        let outcome = app.add_cell(
            &mut spaces,
            AddCellRequest {
                space_id: 5,
                cells: vec![CellInfo {
                    space_id: 5,
                    app_addr: 11,
                    node: CellInfoNode(0),
                    rect: (0.0, 0.0, 0.0, 0.0),
                    is_delete_pending: false,
                }],
                is_first_cell: false,
                is_from_db: false,
                space_data: vec![],
            },
        );
        assert_eq!(
            outcome,
            AddCellOutcome {
                no_cell_here: true,
                ..Default::default()
            }
        );
        assert_eq!(app.failed_adds, vec![5u32]);
        assert!(app.cells.is_empty());
        assert!(
            app.find_space(&spaces, 5).is_some(),
            "the space is created before the failure"
        );
        assert!(app.find_cell(&spaces, 5).is_none());
    }

    /// Only the first cell of the default space on a useDefaultSpace
    /// CellApp fires `onGetFirstCell`, with its `isFromDB`.
    #[test]
    fn the_first_cell_of_the_default_space_starts_the_app() {
        let mut spaces = BTreeMap::new();
        let mut app = CellApp::with_address(APP_ADDR);
        app.start();
        app.set_use_default_space(true);
        let tree = vec![cell_info(1, 0)];

        let outcome = app.add_cell(
            &mut spaces,
            AddCellRequest {
                space_id: 1,
                cells: tree.clone(),
                is_first_cell: true,
                is_from_db: true,
                space_data: vec![],
            },
        );
        assert!(outcome.first_cell_delivered);
        assert_eq!(app.first_cell_deliveries, vec![true]);

        // Not the first cell: no delivery.
        let outcome = app.add_cell(
            &mut spaces,
            AddCellRequest {
                space_id: 1,
                cells: tree.clone(),
                is_first_cell: false,
                is_from_db: true,
                space_data: vec![],
            },
        );
        assert!(!outcome.first_cell_delivered);

        // First cell of another space: still no delivery.
        let outcome = app.add_cell(
            &mut spaces,
            AddCellRequest {
                space_id: 2,
                cells: vec![cell_info(2, 1)],
                is_first_cell: true,
                is_from_db: false,
                space_data: vec![],
            },
        );
        assert!(!outcome.first_cell_delivered);
        assert_eq!(app.first_cell_deliveries.len(), 1);

        // useDefaultSpace off: no delivery either.
        let mut plain_spaces = BTreeMap::new();
        let mut plain = CellApp::with_address(APP_ADDR);
        plain.start();
        let outcome = plain.add_cell(
            &mut plain_spaces,
            AddCellRequest {
                space_id: 1,
                cells: tree,
                is_first_cell: true,
                is_from_db: true,
                space_data: vec![],
            },
        );
        assert!(!outcome.first_cell_delivered);
        assert!(plain.first_cell_deliveries.is_empty());
    }

    /// Destroying the cell through the empty-space chain keeps the manager
    /// and the CellApp agreeing: the registry forgets the cell, the space
    /// outlives it, and the ledger still matches through its destroyed
    /// cells.
    #[test]
    fn destroying_the_cell_leaves_manager_and_cellapp_agreeing() {
        let mut spaces = BTreeMap::new();
        let mut app = CellApp::with_address(APP_ADDR);
        app.start();
        let mgr = MgrLedger {
            spaces: vec![5],
            assignments: vec![(5, APP_ADDR, CellInfoNode(0))],
        };
        app.add_cell(
            &mut spaces,
            AddCellRequest {
                space_id: 5,
                cells: vec![cell_info(5, 0)],
                is_first_cell: false,
                is_from_db: false,
                space_data: vec![],
            },
        );
        assert!(mgr.agrees_with(&app, &spaces));

        // The 01-01 chain empties the space; the timer width destroys the
        // cell.
        {
            let space = spaces.get_mut(&5).expect("space");
            space.add_entity(entity(1, true));
            let e = space.entities()[0].clone();
            space.remove_entity(&e, &mut app);
            space.shut_down_space(0);
            space.tick(1_000_000, &mut app);
        }

        assert!(app.cells.is_empty(), "cells_ forgot the destroyed cell");
        assert_eq!(app.destroyed_cell_nodes, vec![CellInfoNode(0)]);
        assert!(app.find_cell(&spaces, 5).is_none());
        assert!(
            app.find_space(&spaces, 5).is_some(),
            "the space outlives its cell"
        );
        assert!(
            mgr.agrees_with(&app, &spaces),
            "destroyed cells still count as agreement"
        );
    }

    /// The ledger refuses to agree with a CellApp that does not hold a
    /// space the manager created, or that lacks a cell the manager
    /// assigned to it.
    #[test]
    fn the_ledger_refuses_a_cellapp_that_disagrees() {
        let mut spaces = BTreeMap::new();
        let mut app = CellApp::with_address(APP_ADDR);
        app.start();
        app.add_cell(
            &mut spaces,
            AddCellRequest {
                space_id: 5,
                cells: vec![cell_info(5, 0)],
                is_first_cell: false,
                is_from_db: false,
                space_data: vec![],
            },
        );

        // A ledger naming a space this CellApp does not hold disagrees.
        let unknown_space = MgrLedger {
            spaces: vec![6],
            assignments: vec![],
        };
        assert!(!unknown_space.agrees_with(&app, &spaces));

        // A ledger assigning this CellApp a cell it neither holds nor has
        // destroyed disagrees.
        let stranger = MgrLedger {
            spaces: vec![5],
            assignments: vec![(5, APP_ADDR, CellInfoNode(1))],
        };
        assert!(!stranger.agrees_with(&app, &spaces));
    }

    /// The acceptance face: entries land in their own space and nowhere
    /// else — the same entry id and key on another space is a different
    /// entry.
    #[test]
    fn space_data_does_not_leak_between_spaces() {
        let mut spaces_a = BTreeMap::new();
        let mut app = CellApp::with_address(APP_ADDR);
        app.start();
        app.add_cell(
            &mut spaces_a,
            AddCellRequest {
                space_id: 5,
                cells: vec![cell_info(5, 0)],
                is_first_cell: false,
                is_from_db: false,
                space_data: vec![],
            },
        );
        let (mut spaces_b, mut app_b) = (BTreeMap::new(), CellApp::with_address(APP_ADDR));
        app_b.start();
        app_b.add_cell(
            &mut spaces_b,
            AddCellRequest {
                space_id: 6,
                cells: vec![cell_info(6, 0)],
                is_first_cell: false,
                is_from_db: false,
                space_data: vec![],
            },
        );

        spaces_a
            .get_mut(&5)
            .expect("space")
            .read_data_from_stream(&mut app, vec![(7, 300, vec![10]), (8, 301, vec![20])]);
        spaces_b
            .get_mut(&6)
            .expect("space")
            .read_data_from_stream(&mut app_b, vec![(9, 300, vec![30])]);

        let a = spaces_a.get(&5).expect("space");
        let b = spaces_b.get(&6).expect("space");
        assert_eq!(a.space_data.size(), 2, "only this space's two entries");
        assert_eq!(b.space_data.size(), 1, "only this space's one entry");
        assert_eq!(
            a.space_data.data_retrieve_specific(7),
            DataValue {
                key: 300,
                data: vec![10]
            }
        );
        assert_eq!(
            a.space_data.data_retrieve_specific(9),
            DataValue::none(),
            "no leaking in"
        );
        assert_eq!(
            b.space_data.data_retrieve_specific(9),
            DataValue {
                key: 300,
                data: vec![30]
            }
        );
        assert_eq!(
            b.space_data.data_retrieve_specific(7),
            DataValue::none(),
            "no leaking in"
        );
        assert_eq!(
            a.space_data.data_retrieve_first(300).map(|v| v.key),
            Some(300u16),
            "the first entry carrying the key"
        );
        let ids: Vec<_> = a.space_data.entries().map(|(id, _)| id).collect();
        assert_eq!(ids, vec![7u32, 8]);
    }

    /// `addDataEntry`'s novel-only rule and the whole retrieval surface.
    #[test]
    fn an_entry_is_added_only_when_novel() {
        let mut mapping = SpaceDataMapping::default();
        assert!(mapping.add_data_entry(7, 1, vec![10]));
        assert!(
            !mapping.add_data_entry(7, 2, vec![20]),
            "the id already stands"
        );
        assert_eq!(
            mapping.data_retrieve_specific(7),
            DataValue {
                key: 1,
                data: vec![10]
            }
        );
        assert_eq!(
            mapping.data_retrieve_specific_ptr(7).unwrap().data,
            vec![10]
        );
        assert!(mapping.data_retrieve_specific_ptr(8).is_none());
        assert_eq!(mapping.data_retrieve_first(1).unwrap().key, 1);
        assert!(mapping.data_retrieve_first(9).is_none());
        assert_eq!(mapping.size(), 1);

        let (deleted, old) = mapping.del_data_entry_value(7);
        assert!(deleted);
        assert_eq!(
            old,
            DataValue {
                key: 1,
                data: vec![10]
            }
        );
        assert!(!mapping.del_data_entry(8), "no such entry");
        assert_eq!(mapping.size(), 0);

        // Deleting nothing reports nothing, with the invalid value.
        let (gone, missing) = mapping.del_data_entry_value(8);
        assert!(!gone);
        assert_eq!(missing, DataValue::none());

        // `Space::reuse` clears the mapping wholesale.
        mapping.add_data_entry(9, 3, vec![30]);
        mapping.clear();
        assert!(mapping.entries().next().is_none());
        assert_eq!(DataValue::none(), DataValue::none());
        assert!(!DataValue::none().valid());
        assert!(
            DataValue {
                key: 1,
                data: vec![]
            }
            .valid()
        );
    }

    /// An entry deleted with `key == -1` may not be added back until the
    /// next reuse — the reference's recently-deleted guard. Both the add
    /// and the revocation of a user-visible entry land in `recentData_`.
    #[test]
    fn a_recently_deleted_entry_cannot_be_added_back() {
        let mut app = CellApp::new();
        app.start();
        let mut space = Space::new(5);
        assert!(space.space_data_entry(
            &mut app,
            7,
            300,
            vec![10],
            UpdateCellAppMgr::DontUpdate,
            DataEffected::NeedToEffect
        ));
        assert!(space.space_data_entry(
            &mut app,
            7,
            NO_SPACE_DATA_KEY,
            vec![],
            UpdateCellAppMgr::DontUpdate,
            DataEffected::NeedToEffect
        ));
        assert!(space.space_data.is_empty());
        assert_eq!(
            space.recent_data,
            vec![(7u32, 300), (7u32, NO_SPACE_DATA_KEY)]
        );
        assert_eq!(space.end_data_seq, 2, "both sides of the entry count");

        assert!(
            !space.space_data_entry(
                &mut app,
                7,
                300,
                vec![20],
                UpdateCellAppMgr::DontUpdate,
                DataEffected::NeedToEffect
            ),
            "a re-add after deletion is refused"
        );
        assert_eq!(app.rejected_readds, vec![7]);
        assert!(space.space_data.is_empty());

        // A different entry id is not refused.
        assert!(space.space_data_entry(
            &mut app,
            8,
            300,
            vec![30],
            UpdateCellAppMgr::DontUpdate,
            DataEffected::NeedToEffect
        ));
        assert_eq!(space.space_data.size(), 1);

        // `Space::reuse` drops the recent deletions, so the entry may
        // come back.
        space.reuse();
        assert!(space.recent_data.is_empty());
        assert!(space.space_data_entry(
            &mut app,
            7,
            300,
            vec![40],
            UpdateCellAppMgr::DontUpdate,
            DataEffected::NeedToEffect
        ));
        assert_eq!(app.rejected_readds, vec![7], "the refusal was not repeated");
    }

    /// A topology change that collapses a space to one cell gives it the
    /// shutdown when the space is empty.
    #[test]
    fn a_topology_change_to_one_cell_asks_for_the_shut_down() {
        let mut app = CellApp::with_address(APP_ADDR);
        app.start();
        let mut spaces = BTreeMap::new();
        app.add_cell(
            &mut spaces,
            AddCellRequest {
                space_id: 5,
                cells: vec![cell_info(5, 0), cell_info(5, 1)],
                is_first_cell: false,
                is_from_db: false,
                space_data: vec![],
            },
        );
        // The delivery itself re-checked the shutdown and found a
        // two-cell space, so nothing was asked for.
        assert!(
            app.shut_down_requests.is_empty(),
            "a two-cell delivery does not reclaim"
        );
        assert!(app.destroyed_cell_nodes.is_empty());

        let report = spaces
            .get_mut(&5)
            .expect("space")
            .update_geometry(&mut app, vec![cell_info(5, 0)]);
        assert_eq!(report.deleted, vec![CellInfoNode(1)]);
        assert!(!report.own_cell_gone);
        assert!(report.cell_checked_off);
        assert!(report.checked_for_shut_down);
        assert!(report.requested_shut_down, "a collapsed empty space asked");
        assert_eq!(app.shut_down_requests, vec![5]);
        assert_eq!(app.cells.len(), 1, "our own cell was not destroyed");

        // A single-cell space's change re-checks nothing.
        let report = spaces
            .get_mut(&5)
            .expect("space")
            .update_geometry(&mut app, vec![cell_info(5, 0)]);
        assert!(report.deleted.is_empty());
        assert!(!report.checked_for_shut_down, "single-cell, so no re-check");
    }

    /// A topology change that keeps several cells re-checks the shutdown
    /// and finds it refused, and a change that drops our own cell reports
    /// it as going without destroying it.
    #[test]
    fn a_topology_change_that_keeps_the_cells_checks_the_offloads() {
        let mut app = CellApp::with_address(APP_ADDR);
        app.start();
        let mut spaces = BTreeMap::new();
        app.add_cell(
            &mut spaces,
            AddCellRequest {
                space_id: 5,
                cells: vec![cell_info(5, 0), cell_info(5, 1)],
                is_first_cell: false,
                is_from_db: false,
                space_data: vec![],
            },
        );

        // Drop node 1, keep our node 0, add node 2: still two cells.
        let report = spaces
            .get_mut(&5)
            .expect("space")
            .update_geometry(&mut app, vec![cell_info(5, 0), cell_info(5, 2)]);
        assert_eq!(report.deleted, vec![CellInfoNode(1)]);
        assert!(!report.own_cell_gone);
        assert!(report.cell_checked_off);
        assert!(report.checked_for_shut_down);
        assert!(!report.requested_shut_down, "still multi-cell, not empty");

        // Now drop our own cell: it is going, not destroyed.
        let report = spaces
            .get_mut(&5)
            .expect("space")
            .update_geometry(&mut app, vec![cell_info(5, 2)]);
        assert_eq!(report.deleted, vec![CellInfoNode(0)]);
        assert!(report.own_cell_gone);
        assert!(!report.cell_checked_off, "nothing to check off");
        assert!(report.checked_for_shut_down);
        assert!(!report.requested_shut_down);
        assert_eq!(app.cells.len(), 1, "a going cell is not destroyed");
    }

    /// An entry headed for the CellAppMgr is sent *and* stored — both
    /// branches fall through to the mapping — and a cellless space's own
    /// cell is never "going".
    #[test]
    fn a_manager_bound_entry_is_sent_and_also_stored() {
        let mut app = CellApp::new();
        app.start();
        let mut space = Space::new(5);

        assert!(space.space_data_entry(
            &mut app,
            7,
            300,
            vec![10],
            UpdateCellAppMgr::Update,
            DataEffected::NeedToEffect
        ));
        assert_eq!(app.space_data_updates, vec![(5u32, 7, 300)]);
        assert_eq!(
            space.space_data.size(),
            1,
            "the forwarded entry is stored here too"
        );
        // A forwarded revocation erases the local entry as well.
        assert!(space.space_data_entry(
            &mut app,
            7,
            NO_SPACE_DATA_KEY,
            vec![],
            UpdateCellAppMgr::Update,
            DataEffected::NeedToEffect
        ));
        assert!(space.space_data.is_empty());
        assert_eq!(
            app.space_data_updates.last().copied(),
            Some((5, 7, NO_SPACE_DATA_KEY))
        );

        // A space without a cell is not a single-cell space, so a topology
        // change re-checks the shutdown and nothing is going.
        let report = space.update_geometry(&mut app, vec![cell_info(5, 0)]);
        assert!(report.deleted.is_empty());
        assert!(!report.own_cell_gone);
        assert!(!report.cell_checked_off);
        assert!(
            report.checked_for_shut_down,
            "not single-cell, so it re-checked"
        );
        assert!(!report.requested_shut_down, "no cell means no single cell");
    }

    /// A geometry entry's wire payload: the `SpaceData_MappingData` matrix
    /// and the path that follows it.
    fn geometry_payload(path: &str) -> Vec<u8> {
        let mut payload = vec![0u8; SPACE_DATA_MAPPING_MATRIX_LEN];
        payload.extend_from_slice(path.as_bytes());
        payload
    }

    /// The mount: a constructed space carries a chunk backend, the space
    /// is its only holder, and destroying the space releases that one
    /// reference.
    #[test]
    fn the_mounted_backend_is_the_space_s_only_reference() {
        let mut space = Space::new(5);
        let physical = space.physical().expect("a chunk backend is mounted");
        assert!(
            physical.borrow().as_delegate().is_none(),
            "the default mount is not the delegate backend"
        );
        assert_eq!(
            Rc::strong_count(&physical),
            2,
            "the space holds the only other reference"
        );
        let weak = Rc::downgrade(&physical);
        drop(physical);

        space.destroy();
        assert!(weak.upgrade().is_none(), "the reference was released");
    }

    /// A geometry entry mounts into the physical space as a pending
    /// mapping; a tick around the cell loads it, while a cellless space's
    /// tick is unload-only and leaves it pending.
    #[test]
    fn mapping_data_mounts_geometry_and_a_tick_loads_it() {
        let (mut app, mut space, _cell) = fixture(7, 0);
        space.set_last_mapped_geometry("old".to_string());

        assert!(space.space_data_entry(
            &mut app,
            3,
            SPACE_DATA_MAPPING_KEY_CLIENT_SERVER,
            geometry_payload("spaces/sets"),
            UpdateCellAppMgr::DontUpdate,
            DataEffected::NeedToEffect
        ));
        assert!(space.should_check_load_bounds, "onNewGeometryMapping ran");
        assert!(
            space.last_mapped_geometry.is_empty(),
            "someone else mapped a geometry, so ours is not the last"
        );
        let physical = space.physical().expect("mounted");
        {
            let borrowed = physical.borrow();
            let chunk = borrowed
                .as_chunk()
                .expect("the default mount is the chunk backend");
            assert_eq!(
                chunk.mappings.get(&3).map(|m| m.path.as_str()),
                Some("spaces/sets")
            );
            assert!(
                chunk.mappings[&3].pending,
                "addMappingAsync is still in flight"
            );
        }

        assert!(space.chunk_tick(), "the tick loads around the cell");
        assert!(
            physical.borrow_mut().update(false),
            "the loaded mapping is still mounted"
        );
        assert!(
            !physical
                .borrow()
                .as_chunk()
                .expect("chunk backend")
                .mappings[&3]
                .pending
        );

        // A space with no cell is already unload-only.
        let mut bare = Space::new(8);
        assert!(bare.space_data_entry(
            &mut app,
            3,
            SPACE_DATA_MAPPING_KEY_CLIENT_SERVER,
            geometry_payload("spaces/sets"),
            UpdateCellAppMgr::DontUpdate,
            DataEffected::NeedToEffect
        ));
        assert!(!bare.chunk_tick(), "no cell, so the tick only unloads");
        let bare_physical = bare.physical().expect("mounted");
        assert!(
            bare_physical
                .borrow()
                .as_chunk()
                .expect("chunk backend")
                .mappings[&3]
                .pending,
            "unload-only does not advance the mapping"
        );
    }

    /// Revoking the geometry entry unloads it from the physical space, and
    /// the space is fully unloaded again.
    #[test]
    fn revoking_the_mapping_unmounts_it() {
        let (mut app, mut space, _cell) = fixture(7, 0);
        assert!(space.space_data_entry(
            &mut app,
            3,
            SPACE_DATA_MAPPING_KEY_CLIENT_SERVER,
            geometry_payload("spaces/sets"),
            UpdateCellAppMgr::DontUpdate,
            DataEffected::NeedToEffect
        ));
        assert!(!space.is_fully_unloaded(), "the mapping is mounted");

        assert!(space.space_data_entry(
            &mut app,
            3,
            NO_SPACE_DATA_KEY,
            vec![],
            UpdateCellAppMgr::DontUpdate,
            DataEffected::NeedToEffect
        ));
        assert!(
            space
                .physical()
                .expect("mounted")
                .borrow()
                .as_chunk()
                .is_none_or(|chunk| chunk.mappings.is_empty())
        );
        assert!(space.is_fully_unloaded(), "nothing is mounted any more");
    }

    /// The acceptance face: the occupant leaves, the manager's message
    /// takes the cell, the mounted geometry is revoked, and the reaper —
    /// `pCell() == NULL && isFullyUnloaded()` — deletes the space, whose
    /// death releases its reference to the backend.
    #[test]
    fn the_reaper_reclaims_a_drained_space_and_releases_the_reference() {
        let mut app = CellApp::with_address(APP_ADDR);
        app.start();
        let mut spaces = BTreeMap::new();
        app.add_cell(
            &mut spaces,
            AddCellRequest {
                space_id: 7,
                cells: vec![cell_info(7, 0)],
                is_first_cell: false,
                is_from_db: false,
                space_data: vec![],
            },
        );
        let physical = spaces.get(&7).expect("space").physical().expect("mounted");
        let weak = Rc::downgrade(&physical);
        drop(physical);

        let space = spaces.get_mut(&7).expect("space");
        let occupant = entity(1, true);
        space.add_entity(occupant.clone());
        assert!(
            !space.is_fully_unloaded(),
            "an occupant keeps the space from being fully unloaded"
        );
        assert!(space.space_data_entry(
            &mut app,
            3,
            SPACE_DATA_MAPPING_KEY_CLIENT_SERVER,
            geometry_payload("spaces/sets"),
            UpdateCellAppMgr::DontUpdate,
            DataEffected::NeedToEffect
        ));
        assert!(!space.is_fully_unloaded());

        // The occupant leaves: the space asks for its shutdown, the
        // manager answers, one timer width runs, and the cell goes.
        space.remove_entity(&occupant, &mut app);
        assert_eq!(app.shut_down_requests, vec![7]);
        assert!(space.shut_down_space(0));
        assert!(space.tick(SHUTDOWN_SPACE_DELAY_US, &mut app));
        assert!(space.cell().is_none());
        assert_eq!(app.destroyed_cell_nodes, vec![CellInfoNode(0)]);

        // Revoking the mapping drains the backend; with no cell the tick
        // is unload-only, so nothing new mounts.
        assert!(space.space_data_entry(
            &mut app,
            3,
            NO_SPACE_DATA_KEY,
            vec![],
            UpdateCellAppMgr::DontUpdate,
            DataEffected::NeedToEffect
        ));
        assert!(!space.chunk_tick(), "unload-only loads nothing");
        assert!(space.is_fully_unloaded());

        let deleted = delete_old_spaces(&mut spaces);
        assert_eq!(deleted, vec![7]);
        assert!(spaces.is_empty());
        assert!(weak.upgrade().is_none(), "the reference was released");
    }

    /// The delegate backend: the mount builds a physics delegate, the
    /// worlds materialise on ticks, and tearing the space down releases
    /// everything through the factory — the physics delegate only in the
    /// destructor.
    #[test]
    fn the_delegate_backend_releases_every_reference() {
        let factory = Rc::new(RefCell::new(SpaceWorldFactory::default()));
        let mut app = CellApp::with_address(APP_ADDR);
        app.use_game_delegate = true;
        app.world_factory = Rc::clone(&factory);
        app.start();
        let mut spaces = BTreeMap::new();
        app.add_cell(
            &mut spaces,
            AddCellRequest {
                space_id: 9,
                cells: vec![cell_info(9, 0)],
                is_first_cell: false,
                is_from_db: false,
                space_data: vec![],
            },
        );
        assert_eq!(
            factory.borrow().physics_delegates_created,
            1,
            "the mount built the physics delegate"
        );
        let physical = spaces.get(&9).expect("space").physical().expect("mounted");
        let weak = Rc::downgrade(&physical);

        let space = spaces.get_mut(&9).expect("space");
        assert!(space.space_data_entry(
            &mut app,
            4,
            SPACE_DATA_MAPPING_KEY_CLIENT_SERVER,
            geometry_payload("worlds/islands"),
            UpdateCellAppMgr::DontUpdate,
            DataEffected::NeedToEffect
        ));
        assert_eq!(factory.borrow().loaded, vec!["worlds/islands".to_string()]);

        // The loading tick materialises the world; a second finds it
        // loaded and asks no more. The delegate tick never reports new
        // columns (the reference's load balancing is a TODO).
        assert!(!space.chunk_tick());
        assert!(!space.chunk_tick());
        assert_eq!(factory.borrow().created, vec!["worlds/islands".to_string()]);
        {
            let borrowed = physical.borrow();
            let delegate = borrowed
                .as_delegate()
                .expect("a delegate-mode CellApp mounts the delegate backend");
            assert_eq!(
                delegate.worlds.get(&4).map(String::as_str),
                Some("worlds/islands")
            );
        }

        // A world whose load will never finish: the path mounts but the
        // world never materialises, so the space is never fully unloaded.
        assert!(space.space_data_entry(
            &mut app,
            5,
            SPACE_DATA_MAPPING_KEY_CLIENT_SERVER,
            geometry_payload("worlds/broken"),
            UpdateCellAppMgr::DontUpdate,
            DataEffected::NeedToEffect
        ));
        assert_eq!(factory.borrow().loaded.len(), 2);

        // The delivered shutdown chain takes the cell away.
        assert!(space.check_for_shut_down(&mut app));
        assert!(space.shut_down_space(0));
        assert!(space.tick(SHUTDOWN_SPACE_DELAY_US, &mut app));
        assert!(space.cell().is_none());

        // Revoking the loaded world drains it, but the broken path holds
        // the space — the reaper refuses.
        assert!(space.space_data_entry(
            &mut app,
            4,
            NO_SPACE_DATA_KEY,
            vec![],
            UpdateCellAppMgr::DontUpdate,
            DataEffected::NeedToEffect
        ));
        assert!(!space.is_fully_unloaded());

        // The reaper refuses: the broken path holds the space.
        assert!(
            !spaces.get(&9).expect("space").is_fully_unloaded(),
            "the broken path holds the space"
        );
        assert!(delete_old_spaces(&mut spaces).is_empty());

        // Our pointer goes back; only the space's reference remains.
        drop(physical);

        // The destructor path clears what is left: the in-flight load is
        // cancelled, the world resources removed, and the physics delegate
        // — created at mount — released only here.
        spaces.get_mut(&9).expect("space").destroy();
        assert!(weak.upgrade().is_none(), "the reference was released");
        let history = factory.borrow();
        assert_eq!(
            history.removed_instances,
            vec!["worlds/islands".to_string()]
        );
        assert_eq!(
            history.removed,
            vec!["worlds/islands".to_string(), "worlds/broken".to_string()]
        );
        assert_eq!(
            history.cancelled,
            vec!["worlds/broken".to_string()],
            "the not-yet-loaded path was cancelled"
        );
        assert_eq!(
            history.physics_delegates_removed, 1,
            "the physics delegate is released only in the destructor"
        );
    }

    /// Tearing a still-mounted delegate space down — the `~CellApp` road:
    /// the in-flight load is cancelled, the loaded world is removed as an
    /// instance and as a resource, and the dead space reports unloaded and
    /// tickless because its backend is already gone.
    #[test]
    fn a_delegate_clear_cancels_and_drains_a_mounted_backend() {
        let factory = Rc::new(RefCell::new(SpaceWorldFactory::default()));
        let mut app = CellApp::with_address(APP_ADDR);
        app.use_game_delegate = true;
        app.world_factory = Rc::clone(&factory);
        app.start();
        let mut spaces = BTreeMap::new();
        app.add_cell(
            &mut spaces,
            AddCellRequest {
                space_id: 9,
                cells: vec![cell_info(9, 0)],
                is_first_cell: false,
                is_from_db: false,
                space_data: vec![],
            },
        );
        let physical = spaces.get(&9).expect("space").physical().expect("mounted");
        let weak = Rc::downgrade(&physical);
        assert!(
            physical.borrow().as_chunk().is_none(),
            "the delegate mode never mounts the chunk backend"
        );

        let space = spaces.get_mut(&9).expect("space");
        assert!(space.space_data_entry(
            &mut app,
            4,
            SPACE_DATA_MAPPING_KEY_CLIENT_SERVER,
            geometry_payload("worlds/islands"),
            UpdateCellAppMgr::DontUpdate,
            DataEffected::NeedToEffect
        ));
        assert!(!space.chunk_tick());
        // A path that never materialises: it stays an in-flight load.
        assert!(space.space_data_entry(
            &mut app,
            5,
            SPACE_DATA_MAPPING_KEY_CLIENT_SERVER,
            geometry_payload("worlds/broken"),
            UpdateCellAppMgr::DontUpdate,
            DataEffected::NeedToEffect
        ));

        space.destroy();
        assert!(
            spaces.get(&9).expect("space").is_fully_unloaded(),
            "a destroyed space is fully unloaded"
        );
        assert!(
            !spaces.get_mut(&9).expect("space").chunk_tick(),
            "a destroyed space has nothing to tick"
        );
        drop(physical);
        assert!(weak.upgrade().is_none(), "the reference was released");

        let history = factory.borrow();
        assert_eq!(
            history.cancelled,
            vec!["worlds/broken".to_string()],
            "only the in-flight load was cancelled"
        );
        assert_eq!(
            history.removed_instances,
            vec!["worlds/islands".to_string()]
        );
        assert_eq!(
            history.removed,
            vec!["worlds/islands".to_string(), "worlds/broken".to_string()]
        );
        assert_eq!(history.physics_delegates_removed, 1);
    }

    /// The delegate entry faces: re-adding a mounted entry reuses its path,
    /// revoking an entry that never mounted is a no-op, revoking a path
    /// whose world never materialised removes no instance, and `reuse`
    /// clears through the factory.
    #[test]
    fn a_delegate_reuses_mounted_paths_and_ignores_unknown_unloads() {
        let factory = Rc::new(RefCell::new(SpaceWorldFactory::default()));
        let mut app = CellApp::with_address(APP_ADDR);
        app.use_game_delegate = true;
        app.world_factory = Rc::clone(&factory);
        app.start();
        let mut spaces = BTreeMap::new();
        app.add_cell(
            &mut spaces,
            AddCellRequest {
                space_id: 9,
                cells: vec![cell_info(9, 0)],
                is_first_cell: false,
                is_from_db: false,
                space_data: vec![],
            },
        );
        let physical = spaces.get(&9).expect("space").physical().expect("mounted");

        let space = spaces.get_mut(&9).expect("space");
        assert!(space.space_data_entry(
            &mut app,
            4,
            SPACE_DATA_MAPPING_KEY_CLIENT_SERVER,
            geometry_payload("worlds/one"),
            UpdateCellAppMgr::DontUpdate,
            DataEffected::NeedToEffect
        ));
        // The Space-level re-add of a live entry is refused (the mapping
        // already has it); the backend's own reuse face drives directly.
        assert!(!space.space_data_entry(
            &mut app,
            4,
            SPACE_DATA_MAPPING_KEY_CLIENT_SERVER,
            geometry_payload("worlds/two"),
            UpdateCellAppMgr::DontUpdate,
            DataEffected::NeedToEffect
        ));
        assert_eq!(factory.borrow().loaded, vec!["worlds/one".to_string()]);
        {
            let mut borrowed = physical.borrow_mut();
            borrowed.load_resource(4, "worlds/two");
            assert_eq!(
                borrowed
                    .as_delegate()
                    .expect("the delegate backend")
                    .paths
                    .get(&4)
                    .map(String::as_str),
                Some("worlds/one"),
                "the mounted path is reused"
            );
        }
        assert_eq!(factory.borrow().loaded, vec!["worlds/one".to_string()]);

        // A mapping recorded without being effected: revoking it later
        // finds nothing mounted in the backend.
        assert!(space.space_data_entry(
            &mut app,
            6,
            SPACE_DATA_MAPPING_KEY_CLIENT_SERVER,
            geometry_payload("worlds/ghost"),
            UpdateCellAppMgr::DontUpdate,
            DataEffected::AlreadyEffected
        ));
        assert_eq!(factory.borrow().loaded.len(), 1);
        assert!(space.space_data_entry(
            &mut app,
            6,
            NO_SPACE_DATA_KEY,
            vec![],
            UpdateCellAppMgr::DontUpdate,
            DataEffected::NeedToEffect
        ));
        assert_eq!(factory.borrow().removed.len(), 0);

        // Revoking the mounted entry before its world materialised: no
        // instance to remove, but the resources go.
        assert!(space.space_data_entry(
            &mut app,
            4,
            NO_SPACE_DATA_KEY,
            vec![],
            UpdateCellAppMgr::DontUpdate,
            DataEffected::NeedToEffect
        ));
        assert_eq!(factory.borrow().removed, vec!["worlds/one".to_string()]);
        assert!(factory.borrow().removed_instances.is_empty());

        // `reuse` clears through the same factory face.
        assert!(space.space_data_entry(
            &mut app,
            7,
            SPACE_DATA_MAPPING_KEY_CLIENT_SERVER,
            geometry_payload("worlds/three"),
            UpdateCellAppMgr::DontUpdate,
            DataEffected::NeedToEffect
        ));
        space.reuse();
        assert!(space.is_fully_unloaded());
        let borrowed = physical.borrow();
        let delegate = borrowed.as_delegate().expect("the delegate backend");
        assert!(delegate.paths.is_empty());
        assert!(delegate.worlds.is_empty());
        assert_eq!(
            factory.borrow().removed,
            vec!["worlds/one".to_string(), "worlds/three".to_string()]
        );
    }

    /// A world whose load fails is dropped from the backend on the next
    /// tick, and the space is fully unloaded again.
    #[test]
    fn a_failed_delegate_load_is_erased_on_the_tick() {
        let factory = Rc::new(RefCell::new(SpaceWorldFactory::default()));
        factory.borrow_mut().fail_world("worlds/broken");
        let mut app = CellApp::with_address(APP_ADDR);
        app.use_game_delegate = true;
        app.world_factory = Rc::clone(&factory);
        app.start();
        let mut spaces = BTreeMap::new();
        app.add_cell(
            &mut spaces,
            AddCellRequest {
                space_id: 9,
                cells: vec![cell_info(9, 0)],
                is_first_cell: false,
                is_from_db: false,
                space_data: vec![],
            },
        );
        let physical = spaces.get(&9).expect("space").physical().expect("mounted");

        let space = spaces.get_mut(&9).expect("space");
        assert!(space.space_data_entry(
            &mut app,
            4,
            SPACE_DATA_MAPPING_KEY_CLIENT_SERVER,
            geometry_payload("worlds/broken"),
            UpdateCellAppMgr::DontUpdate,
            DataEffected::NeedToEffect
        ));
        // A healthy path behind the broken one: the failing pass ends at
        // the break, so the healthy world waits for the next update.
        assert!(space.space_data_entry(
            &mut app,
            5,
            SPACE_DATA_MAPPING_KEY_CLIENT_SERVER,
            geometry_payload("worlds/late"),
            UpdateCellAppMgr::DontUpdate,
            DataEffected::NeedToEffect
        ));
        assert!(!space.is_fully_unloaded());
        assert!(!space.chunk_tick());
        assert_eq!(factory.borrow().created, vec!["worlds/broken".to_string()]);
        assert!(
            !space.is_fully_unloaded(),
            "the healthy path survives the failing pass"
        );
        assert!(!space.chunk_tick());
        assert_eq!(
            factory.borrow().created,
            vec!["worlds/broken".to_string(), "worlds/late".to_string()]
        );
        {
            let borrowed = physical.borrow();
            let delegate = borrowed.as_delegate().expect("the delegate backend");
            assert!(
                !delegate.paths.contains_key(&4),
                "the failed path was erased"
            );
            assert!(delegate.worlds.contains_key(&5));
        }
        assert!(
            !space.is_fully_unloaded(),
            "the healthy world still holds the space"
        );

        // Revoking the entry whose load failed settles the mapping; the
        // backend has nothing of its own to remove for it.
        assert!(space.space_data_entry(
            &mut app,
            4,
            NO_SPACE_DATA_KEY,
            vec![],
            UpdateCellAppMgr::DontUpdate,
            DataEffected::NeedToEffect
        ));
        assert!(factory.borrow().removed.is_empty());
        drop(physical);
        spaces.get_mut(&9).expect("space").destroy();
        assert_eq!(factory.borrow().removed, vec!["worlds/late".to_string()]);
        assert_eq!(
            factory.borrow().removed_instances,
            vec!["worlds/late".to_string()]
        );
        assert_eq!(factory.borrow().physics_delegates_removed, 1);
    }

    /// A space whose backend is already gone - torn down before the
    /// reaper deleted it - still settles its mapping but touches nothing:
    /// mounts, revokes, `reuse` and a second teardown are all no-ops.
    #[test]
    fn a_torn_down_space_ignores_later_space_data() {
        let mut app = CellApp::new();
        app.start();
        let mut space = Space::new(5);
        space.destroy();

        assert!(space.space_data_entry(
            &mut app,
            4,
            SPACE_DATA_MAPPING_KEY_CLIENT_SERVER,
            geometry_payload("spaces/sets"),
            UpdateCellAppMgr::DontUpdate,
            DataEffected::NeedToEffect
        ));
        assert!(space.physical().is_none());
        assert!(space.space_data_entry(
            &mut app,
            4,
            NO_SPACE_DATA_KEY,
            vec![],
            UpdateCellAppMgr::DontUpdate,
            DataEffected::NeedToEffect
        ));
        space.reuse();
        space.destroy();
        assert!(space.is_fully_unloaded());
    }

    /// A truncated artificial-load payload parses as the default of zero.
    #[test]
    fn a_truncated_artificial_load_parses_as_zero() {
        let mut app = CellApp::new();
        app.start();
        let mut space = Space::new(5);

        let mut short = Vec::new();
        short.extend_from_slice(&7u16.to_le_bytes());
        assert!(space.space_data_entry(
            &mut app,
            11,
            SPACE_DATA_ARTIFICIAL_LOAD,
            short,
            UpdateCellAppMgr::DontUpdate,
            DataEffected::NeedToEffect
        ));
        assert_eq!(space.artificial_min_load, 0.0);
    }

    /// The time-of-day and artificial-load entries land in their fields;
    /// `ALREADY_EFFECTED` keeps the entry without acting on it.
    #[test]
    fn time_of_day_and_artificial_load_land_in_the_space() {
        let mut app = CellApp::new();
        app.start();
        let mut space = Space::new(5);

        let mut tod = Vec::new();
        tod.extend_from_slice(&0.25f32.to_le_bytes());
        tod.extend_from_slice(&2.0f32.to_le_bytes());
        assert!(space.space_data_entry(
            &mut app,
            1,
            SPACE_DATA_TOD_KEY,
            tod,
            UpdateCellAppMgr::DontUpdate,
            DataEffected::NeedToEffect
        ));
        assert_eq!(space.initial_time_of_day, 0.25);
        assert_eq!(space.game_seconds_per_second, 2.0);

        // The effect reads the *first* entry carrying the key: revoke it
        // and a short payload next in line resets to the defaults.
        assert!(space.space_data_entry(
            &mut app,
            1,
            NO_SPACE_DATA_KEY,
            vec![],
            UpdateCellAppMgr::DontUpdate,
            DataEffected::NeedToEffect
        ));
        assert_eq!(space.initial_time_of_day, -1.0);
        assert_eq!(space.game_seconds_per_second, 0.0);

        let load = 7.5f32.to_le_bytes().to_vec();
        assert!(space.space_data_entry(
            &mut app,
            3,
            SPACE_DATA_ARTIFICIAL_LOAD,
            load,
            UpdateCellAppMgr::DontUpdate,
            DataEffected::NeedToEffect
        ));
        assert_eq!(space.artificial_min_load, 7.5);

        // ALREADY_EFFECTED: the entry stands, the effect is skipped.
        let mut tod2 = Vec::new();
        tod2.extend_from_slice(&0.75f32.to_le_bytes());
        tod2.extend_from_slice(&3.0f32.to_le_bytes());
        assert!(space.space_data_entry(
            &mut app,
            5,
            SPACE_DATA_TOD_KEY,
            tod2,
            UpdateCellAppMgr::DontUpdate,
            DataEffected::AlreadyEffected
        ));
        assert_eq!(space.initial_time_of_day, -1.0, "the effect was skipped");
        assert_eq!(
            space.space_data.data_retrieve_specific(5).data.len(),
            8,
            "the entry stands"
        );
    }

    /// A duplicate add and a delete of nothing are both refused — the
    /// effective key stays unset ("attempted to add or delete entry
    /// twice!") — and neither is the recently-deleted guard.
    #[test]
    fn a_refused_duplicate_or_missing_delete_returns_false() {
        let mut app = CellApp::new();
        app.start();
        let mut space = Space::new(5);
        assert!(space.space_data_entry(
            &mut app,
            7,
            300,
            vec![10],
            UpdateCellAppMgr::DontUpdate,
            DataEffected::NeedToEffect
        ));

        assert!(
            !space.space_data_entry(
                &mut app,
                7,
                301,
                vec![20],
                UpdateCellAppMgr::DontUpdate,
                DataEffected::NeedToEffect
            ),
            "the id already stands"
        );
        assert_eq!(
            space.space_data.data_retrieve_specific(7),
            DataValue {
                key: 300,
                data: vec![10]
            },
            "the first entry is untouched"
        );
        assert!(
            !space.space_data_entry(
                &mut app,
                9,
                NO_SPACE_DATA_KEY,
                vec![],
                UpdateCellAppMgr::DontUpdate,
                DataEffected::NeedToEffect
            ),
            "there is nothing to delete"
        );
        assert!(
            app.rejected_readds.is_empty(),
            "the guard was not the refuser"
        );
        assert_eq!(space.end_data_seq, 1, "the refused calls recorded nothing");

        assert!(space.space_data_entry(
            &mut app,
            7,
            NO_SPACE_DATA_KEY,
            vec![],
            UpdateCellAppMgr::DontUpdate,
            DataEffected::NeedToEffect
        ));
        assert!(space.space_data.is_empty());
    }

    /// Cell-only keys are outside `recentData_` and the re-add guard: a
    /// deleted cell-only entry may be added straight back.
    #[test]
    fn a_cell_only_entry_skips_the_recent_guard() {
        let mut app = CellApp::new();
        app.start();
        let mut space = Space::new(5);
        assert!(space.space_data_entry(
            &mut app,
            7,
            20000,
            vec![10],
            UpdateCellAppMgr::DontUpdate,
            DataEffected::NeedToEffect
        ));
        assert!(space.space_data_entry(
            &mut app,
            7,
            NO_SPACE_DATA_KEY,
            vec![],
            UpdateCellAppMgr::DontUpdate,
            DataEffected::NeedToEffect
        ));
        assert!(
            space.recent_data.is_empty(),
            "cell-only entries stay out of recentData_"
        );
        assert_eq!(space.end_data_seq, 0);

        assert!(
            space.space_data_entry(
                &mut app,
                7,
                20001,
                vec![20],
                UpdateCellAppMgr::DontUpdate,
                DataEffected::NeedToEffect
            ),
            "no guard on cell-only keys"
        );
        assert_eq!(space.space_data.data_retrieve_specific(7).key, 20001);
    }

    /// `Space::reuse` reuses the mounted backend: the mappings drain, the
    /// clear is marked, and — unlike `clear()` — the mapping factory stays
    /// attached.
    #[test]
    fn reuse_drains_the_mounted_backend_but_keeps_its_factory() {
        let (mut app, mut space, _cell) = fixture(7, 0);
        assert!(space.space_data_entry(
            &mut app,
            3,
            SPACE_DATA_MAPPING_KEY_CLIENT_SERVER,
            geometry_payload("spaces/sets"),
            UpdateCellAppMgr::DontUpdate,
            DataEffected::NeedToEffect
        ));

        space.reuse();
        let physical = space.physical().expect("mounted");
        let borrowed = physical.borrow();
        let chunk = borrowed
            .as_chunk()
            .expect("the default mount is the chunk backend");
        assert!(chunk.mappings.is_empty(), "reuse drained the mappings");
        assert!(chunk.chunk_space.cleared);
        assert!(
            chunk.chunk_space.mapping_factory_attached,
            "only clear() severs the mapping factory"
        );
        assert!(space.space_data.is_empty(), "the data face went with it");
    }

    /// The reference's `Space::removeEntity` ordering: the entity's own
    /// triggers come down first (the contract-to-zero firing the leave
    /// for everything inside), then the node itself sweeps out through
    /// the maximum Z.
    #[test]
    fn remove_entity_tears_down_the_range_trigger_before_the_node() {
        use crate::cellapp_range_list::{
            FLAG_LOWER_AOI_TRIGGER, FLAG_NO_TRIGGERS, FLAG_UPPER_AOI_TRIGGER, TriggerTarget,
        };

        let (mut app, mut space, _cell) = fixture(3, 0);

        let central = entity(10, true);
        space.add_entity(Rc::clone(&central));
        let central_node = app
            .range_lists
            .entry(3)
            .or_default()
            .add_entity_node(Rc::clone(&central));
        central.borrow_mut().range_node = Some(central_node);

        let watchee = entity(11, true);
        watchee.borrow_mut().position = [2.0, 0.0, 0.0];
        space.add_entity(Rc::clone(&watchee));
        let watchee_node = app
            .range_lists
            .entry(3)
            .or_default()
            .add_entity_node(Rc::clone(&watchee));
        watchee.borrow_mut().range_node = Some(watchee_node);

        // The witness trigger on the central entity, with the watchee
        // inside its area.
        let events: Rc<RefCell<Vec<(u32, bool)>>> = Rc::new(RefCell::new(Vec::new()));
        let list = app.range_lists.get_mut(&3).expect("space 3");
        let trigger = list.create_trigger(
            central_node,
            5.0,
            FLAG_LOWER_AOI_TRIGGER,
            FLAG_UPPER_AOI_TRIGGER,
            FLAG_NO_TRIGGERS,
            FLAG_NO_TRIGGERS,
            TriggerTarget::Recorder(Rc::clone(&events)),
        );
        list.trigger_insert(trigger);
        list.entity_add_trigger(&central, trigger);
        assert_eq!(*events.borrow(), vec![(11u32, true)]);

        space.remove_entity(&central, &mut app);
        assert_eq!(
            *events.borrow(),
            vec![(11, true), (11, false)],
            "the trigger contracts to a point on the way down"
        );
        assert!(central.borrow().range_node.is_none());
        assert!(central.borrow().range_triggers.is_empty());
        assert!(app.range_lists.get(&3).expect("space 3").is_sorted());
        // The chains hold the watchee's node and the tail terminator.
        assert_eq!(app.range_lists.get(&3).expect("space 3").x_order().len(), 2);
    }
}
