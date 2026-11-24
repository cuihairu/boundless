//! BW-08-03-02: Witness and EntityCache management.
//!
//! Port of the AoI-management face of `cellapp/witness.cpp` plus
//! `cellapp/entity_cache.{hpp,cpp,ipp}`. The Witness is created per
//! client-backed RealEntity and keeps one `EntityCache` per observed entity
//! in `aoiMap_`, driven through a priority queue (`entityQueue_`).
//!
//! State machine per cache (flags in `entity_cache.hpp`):
//! ENTER_PENDING -> (sendEnter) REQUEST_PENDING -> (requestEntityUpdate)
//! CREATE_PENDING -> (sendCreate) updatable; leaveAoI marks GONE and the
//! update pass deletes it from client and map. A cache that re-enters before
//! the deletion pass runs is `reuse()`d; if its last event was trimmed off the
//! entity's history it takes the REFRESH path (delete-from-client then
//! re-enter) so first observation and re-entry agree.
//!
//! Scope: cache lifecycle, manual/trigger pairing, withheld handling, the
//! 256-entry ID-alias pool, leave stamps, vehicle-change sync and the default
//! AoI priority scheme accumulation. Bandwidth/priority selection of updates
//! is BW-08-03-03; history/property payloads are BW-08-03-04.

use std::collections::HashMap;

/// `EntityID` on the cellapp side.
pub type EntityId = u32;

/// `NO_ID_ALIAS` — reserved value that is never handed out (`entity_cache.hpp`).
pub const NO_ID_ALIAS: u8 = 0xff;

/// `EntityCache::MAX_LOD_LEVELS`.
pub const MAX_LOD_LEVELS: usize = 4;

/// `DEFAULT_AOI_RADIUS` in `cellapp_config.cpp`; also the `maxAoIRadius`.
pub const DEFAULT_AOI_RADIUS: f32 = 500.0;

/// `CellAppConfig::witnessUpdateDeltaGrowthThrottle` default: 1 + 1/8.
pub const WITNESS_UPDATE_DELTA_GROWTH_THROTTLE: f64 = 1.0 + 1.0 / 8.0;

/// `CellAppConfig::witnessUpdateMaxPriorityDelta` default (`witness.cpp`).
pub const MAX_PRIORITY_DELTA: f64 = 5.0;
/// `CellAppConfig::sendDetailedPlayerVehicles` (default false).
pub const SEND_DETAILED_PLAYER_VEHICLES: bool = false;

/// `UDP_OVERHEAD` (`basictypes.hpp`).
pub const UDP_OVERHEAD: i32 = 28;

/// `NETWORK_BITS_PER_BYTE` (`basictypes.hpp`).
pub const NETWORK_BITS_PER_BYTE: i32 = 8;

/// Stand-in encoded size of one entity update while BW-08-03-04 has not yet
/// brought the real property payloads (the reference writes variable-length
/// position/property data here).
// Flag bits, verbatim from `entity_cache.hpp` `enum // Flags bits`.
pub const ENTER_PENDING: u16 = 1 << 0;
pub const REQUEST_PENDING: u16 = 1 << 1;
pub const CREATE_PENDING: u16 = 1 << 2;
pub const GONE: u16 = 1 << 3;
pub const WITHHELD: u16 = 1 << 4;
pub const REFRESH: u16 = 1 << 5;
pub const PRIORITISED: u16 = 1 << 6;
pub const MANUALLY_ADDED: u16 = 1 << 7;
pub const ADDED_BY_TRIGGER: u16 = 1 << 8;
pub const IS_ALWAYS_DETAILED: u16 = 1 << 9;
/// If any of these are set we shouldn't do a normal update.
pub const NOT_UPDATABLE: u16 =
    ENTER_PENDING | REQUEST_PENDING | CREATE_PENDING | GONE | WITHHELD | REFRESH;
/// Client related state, reset when the entity is removed from the client.
pub const CLIENT_STATE: u16 = ENTER_PENDING | REQUEST_PENDING | CREATE_PENDING | GONE | REFRESH;

/// Snapshot of an observed entity — the parts of `Entity` the Witness reads.
/// The harness keeps the authoritative copy; the Witness looks entities up by
/// id the way `CellApp::findEntity` serves `Witness::requestEntityUpdate`.
#[derive(Debug, Clone, PartialEq)]
pub struct EntitySnapshot {
    pub id: EntityId,
    pub client_type_id: u16,
    /// `position()` — only x/z feed the priority scheme; y rides along in
    /// create payloads.
    pub position: (f32, f32, f32),
    /// `localDirection()`.
    pub yaw: f32,
    pub pitch: f32,
    pub roll: f32,
    /// `pVehicle()` — None when the entity is on the ground.
    pub vehicle_id: Option<EntityId>,
    /// `volatileInfo()` — gates alias allocation, the volatile create
    /// message and the projection detail.
    pub volatile: VolatileInfo,
    /// `isOnGround()` — on-ground entities pack only x/z.
    pub is_on_ground: bool,
    /// `pReal()->controlledByRef().id == entity_.id()` — the witness's own
    /// client drives this entity, so it already knows the position.
    pub controlled_by_self: bool,
    /// `eventHistory()` in chronological order.
    pub events: Vec<HistoryEvent>,
    /// `lodLevels()` transitions; empty means the entity never changes
    /// detail level.
    pub lod_thresholds: Vec<LodLevelThreshold>,
    /// `lastEventNumber()`.
    pub last_event_number: i32,
    /// `volatileUpdateNumber()`.
    pub volatile_update_number: u16,
    /// `vehicleChangeNum()`.
    pub vehicle_change_num: u8,
    /// `eventHistory().lastTrimmedEventNumber()` — events older than this are
    /// gone, and a cache behind it must refresh instead of reuse.
    pub last_trimmed_event_number: i32,
    /// `pType()->description().lodLevels().size()`.
    pub lod_levels: u8,
    /// `pType()->description().canBeOnClient()`.
    pub can_be_on_client: bool,
    pub destroyed: bool,
    /// `aoiUpdateSchemeID()`.
    pub aoi_update_scheme_id: u8,
}

impl EntitySnapshot {
    /// A volatile ground entity with no history — enough for most fixtures.
    pub fn new(id: EntityId, client_type_id: u16, position: (f32, f32, f32)) -> Self {
        EntitySnapshot {
            id,
            client_type_id,
            position,
            yaw: 0.0,
            pitch: 0.0,
            roll: 0.0,
            vehicle_id: None,
            volatile: VolatileInfo::ALWAYS_ALL,
            is_on_ground: true,
            controlled_by_self: false,
            events: Vec::new(),
            lod_thresholds: Vec::new(),
            last_event_number: 0,
            volatile_update_number: 0,
            vehicle_change_num: 0,
            last_trimmed_event_number: 0,
            lod_levels: 1,
            can_be_on_client: true,
            destroyed: false,
            aoi_update_scheme_id: 0,
        }
    }
    /// `volatileInfo().hasVolatile( 0.f )`.
    pub fn has_volatile(&self) -> bool {
        self.volatile.has_volatile(0.0)
    }
}

/// One AoI update scheme (`aoi_update_schemes.hpp`). The default scheme is
/// initialised with min delta 1, max delta 101 over the 500m max radius,
/// which works out to `(dist/5 + 1) * 1`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AoIUpdateScheme {
    weighting: f32,
    distance_weighting: f32,
}

impl AoIUpdateScheme {
    /// `AoIUpdateScheme::init` — non-positive minimum makes the scheme
    /// "coincident": everything sits at priority delta 1.
    pub fn init(min_delta: f32, max_delta: f32, max_aoi_radius: f32) -> Self {
        if min_delta <= 0.0 || max_delta < min_delta {
            AoIUpdateScheme {
                weighting: 0.0,
                distance_weighting: 0.0,
            }
        } else {
            AoIUpdateScheme {
                weighting: min_delta,
                distance_weighting: ((max_delta / min_delta) - 1.0) / max_aoi_radius,
            }
        }
    }

    /// `shouldTreatAsCoincident`.
    pub fn should_treat_as_coincident(&self) -> bool {
        self.weighting == 0.0 && self.distance_weighting == 0.0
    }

    /// `apply` — priority delta for a squared distance.
    /// `AoIUpdateSchemes::shouldTreatAsCoincident` — the coincident init
    /// leaves both weightings at zero.
    pub fn is_coincident(&self) -> bool {
        self.weighting == 0.0 && self.distance_weighting == 0.0
    }

    pub fn apply(&self, distance_squared: f32) -> f64 {
        if self.should_treat_as_coincident() {
            return 1.0;
        }
        let distance = distance_squared.sqrt();
        ((distance * self.distance_weighting + 1.0) * self.weighting) as f64
    }
}

/// Scheme 0: `("default", 1, 101)` over `maxAoIRadius` (`aoi_update_schemes.cpp`).
pub fn default_aoi_scheme() -> AoIUpdateScheme {
    AoIUpdateScheme::init(1.0, 101.0, DEFAULT_AOI_RADIUS)
}

/// `ServerUtil::bpsToPacketSize` — bits per second to payload bytes per
/// packet, minus the UDP/IP header overhead (`lib/server/util.hpp`).
pub fn bps_to_packet_size(bits_per_second: i32, update_hertz: i32) -> i32 {
    bits_per_second / (NETWORK_BITS_PER_BYTE * update_hertz) - UDP_OVERHEAD
}

/// `AoIUpdateSchemes::apply` — scheme 0 is the default; custom schemes come
/// with bw.xml and are treated as coincident until BW-08-03-03 needs them.
/// `VolatileInfo::ALWAYS`.
pub const VOLATILE_ALWAYS: f32 = f32::MAX;

/// Port of `VolatileInfo` — the per-axis volatile thresholds of one entity
/// type, already squared like the reference keeps them after `parse`
/// (`asPriority` squares the configured metres and maps -1 to ALWAYS).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VolatileInfo {
    pub position_priority: f32,
    pub yaw_priority: f32,
    pub pitch_priority: f32,
    pub roll_priority: f32,
}

impl VolatileInfo {
    /// No volatile axis at all — `VolatileInfo()` defaults.
    pub const NONE: VolatileInfo = VolatileInfo {
        position_priority: -1.0,
        yaw_priority: -1.0,
        pitch_priority: -1.0,
        roll_priority: -1.0,
    };

    /// Every axis at maximum rate.
    pub const ALWAYS_ALL: VolatileInfo = VolatileInfo {
        position_priority: VOLATILE_ALWAYS,
        yaw_priority: VOLATILE_ALWAYS,
        pitch_priority: VOLATILE_ALWAYS,
        roll_priority: VOLATILE_ALWAYS,
    };

    /// `VolatileInfo::parse` thresholds in metres; `None` disables the axis
    /// and `Some(-1.0)` means ALWAYS.
    pub fn from_thresholds(
        position: Option<f32>,
        yaw: Option<f32>,
        pitch: Option<f32>,
        roll: Option<f32>,
    ) -> Self {
        let sq = |v: Option<f32>| match v {
            None => -1.0,
            Some(-1.0) => VOLATILE_ALWAYS,
            Some(v) => v * v,
        };
        VolatileInfo {
            position_priority: sq(position),
            yaw_priority: sq(yaw),
            pitch_priority: sq(pitch),
            roll_priority: sq(roll),
        }
    }

    pub fn should_send_position(&self) -> bool {
        self.position_priority > 0.0
    }

    /// `dirType` — how many direction axes are too far away to send.
    pub fn dir_type(&self, lod_priority: f32) -> u8 {
        (lod_priority > self.yaw_priority) as u8
            + (lod_priority > self.pitch_priority) as u8
            + (lod_priority > self.roll_priority) as u8
    }

    /// `hasVolatile` — anything worth sending at this LoD priority?
    pub fn has_volatile(&self, lod_priority: f32) -> bool {
        lod_priority < self.position_priority || lod_priority < self.yaw_priority
    }
}

/// `HistoryEvent::Level` — detail levels for state changes, priorities for
/// messages (already squared like every other threshold in this module).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum HistoryLevel {
    Detail(i32),
    Priority(f32),
}

/// `HistoryEvent` — one entry of the entity's event history.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HistoryEvent {
    pub number: i32,
    pub level: HistoryLevel,
    /// `msgLen()` — what the event charges to the bundle.
    pub payload_bytes: usize,
}

impl HistoryEvent {
    /// `HistoryEvent::shouldSend`.
    pub fn should_send(&self, threshold: f32, detail_level: u8) -> bool {
        match self.level {
            HistoryLevel::Detail(detail) => detail_level as i32 <= detail,
            HistoryLevel::Priority(priority) => threshold < priority,
        }
    }
}

/// One `DataLoDLevel` transition point, thresholds already squared.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LodLevelThreshold {
    pub low: f32,
    pub high: f32,
}

/// How an entity update projects its position onto the client.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PosProjection {
    /// PackedXZ (on ground) or PackedXYZ (airborne), relative to the
    /// witness's reference position.
    Relative {
        x: f32,
        y: f32,
        z: f32,
        on_ground: bool,
    },
    /// PackedXZ/PackedXYZ with the absolute local position — a vehicle
    /// passenger (`writeVolatileDataToBundle` skips the subtraction).
    Absolute {
        x: f32,
        y: f32,
        z: f32,
        on_ground: bool,
    },
    /// `avatarUpdate*Detailed` — absolute local position when the packed
    /// form cannot represent the offset.
    Detailed { x: f32, y: f32, z: f32 },
    /// The type sends no position at all (`shouldSendPosition()` false).
    None,
}

/// How an entity update projects its direction, by `dirType`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum DirProjection {
    YawPitchRoll { yaw: f32, pitch: f32, roll: f32 },
    YawPitch { yaw: f32, pitch: f32 },
    Yaw { yaw: f32 },
    None,
}

fn scheme_for(id: u8) -> AoIUpdateScheme {
    if id == 0 {
        default_aoi_scheme()
    } else {
        AoIUpdateScheme::init(0.0, 0.0, DEFAULT_AOI_RADIUS)
    }
}

/// Messages the Witness writes to the client bundle. Payloads cover the
/// 03-02 face; wire packing and property bodies arrive with BW-08-03-04.
#[derive(Debug, Clone, PartialEq)]
pub enum ClientMessage {
    /// `BaseAppIntInterface::enterAoI`.
    EnterAoI { id: EntityId, id_alias: u8 },
    /// `BaseAppIntInterface::enterAoIOnVehicle`.
    EnterAoIOnVehicle {
        id: EntityId,
        vehicle_id: EntityId,
        id_alias: u8,
    },
    /// `BaseAppIntInterface::leaveAoI` — id then the LoD event stamps.
    LeaveAoI {
        id: EntityId,
        lod_event_numbers: Vec<i32>,
    },
    /// `BaseAppIntInterface::setVehicle` from `writeVehicleChangeToBundle`.
    SetVehicle {
        passenger_id: EntityId,
        vehicle_id: Option<EntityId>,
    },
    /// `BaseAppIntInterface::createEntity` — volatile entity.
    CreateEntity {
        id: EntityId,
        client_type_id: u16,
        position: (f32, f32, f32),
        yaw: f32,
        pitch: f32,
        roll: f32,
    },
    /// `BaseAppIntInterface::createEntityDetailed` — non-volatile entity.
    CreateEntityDetailed {
        id: EntityId,
        client_type_id: u16,
        position: (f32, f32, f32),
        yaw: f32,
        pitch: f32,
        roll: f32,
    },
    /// `BaseAppIntInterface::relativePositionArgs`.
    RelativePosition { position: (f32, f32, f32) },
    /// `BaseAppIntInterface::relativePositionReferenceArgs`.
    RelativePositionReference { sequence_number: u32 },
    /// `BaseAppIntInterface::avatarUpdatePlayerDetailed` — the witness's
    /// own projection, sent ahead of every observed entity.
    AvatarUpdatePlayerDetailed {
        position: (f32, f32, f32),
        yaw: f32,
        pitch: f32,
        roll: f32,
    },
    /// `BaseAppIntInterface::selectEntityArgs`.
    SelectEntity { id: EntityId },
    /// `BaseAppIntInterface::selectAliasedEntityArgs`.
    SelectAliasedEntity { id_alias: u8 },
    /// `BaseAppIntInterface::updateEntity` — the changed-properties resend
    /// that follows a detail-level increase.
    UpdateProperties {
        id: EntityId,
        id_alias: Option<u8>,
        detail_level: u8,
    },
    /// One entity update from `Witness::sendQueueElement`: the projection
    /// of position and direction onto this witness, plus the history events
    /// that crossed the LoD gate. `bytes` is what it charged to this tick's
    /// packet budget.
    EntityUpdate {
        id: EntityId,
        id_alias: Option<u8>,
        pos: PosProjection,
        dir: DirProjection,
        events: Vec<i32>,
        bytes: usize,
    },
}

impl ClientMessage {
    /// Encoded size on the wire — what `Mercury::Bundle::size` would report.
    pub fn wire_size(&self) -> usize {
        match self {
            ClientMessage::EntityUpdate { bytes, .. } => *bytes,
            // The hand-written payloads of this module are small and fixed;
            // only the per-update cost feeds the 03-03 budget gates.
            _ => 8,
        }
    }
}

/// Port of `EntityCache` — what the Witness remembers about one observed
/// entity, including what the client has been told about it.
#[derive(Debug, Clone)]
pub struct EntityCache {
    flags: u16,
    priority: f64,
    last_priority_delta: f64,
    last_event_number: i32,
    last_volatile_update_number: u16,
    detail_level: u8,
    id_alias: u8,
    vehicle_change_num: u8,
    update_scheme_id: u8,
    lod_event_numbers: [i32; MAX_LOD_LEVELS],
}

impl EntityCache {
    /// `EntityCache( const Entity * pEntity )` — constructed with client
    /// state already synced to the entity (via `resetClientState`).
    pub fn new(entity: &EntitySnapshot) -> Self {
        let mut cache = EntityCache {
            flags: 0,
            priority: 0.0,
            last_priority_delta: 0.0,
            last_event_number: 0,
            last_volatile_update_number: 0,
            detail_level: 0,
            id_alias: NO_ID_ALIAS,
            vehicle_change_num: 0,
            update_scheme_id: entity.aoi_update_scheme_id,
            lod_event_numbers: [0; MAX_LOD_LEVELS],
        };
        cache.reset_client_state(entity);
        cache
    }

    /// `resetClientState` — called at construction and when removed from the
    /// client.
    pub fn reset_client_state(&mut self, entity: &EntitySnapshot) {
        self.last_event_number = entity.last_event_number;
        self.last_volatile_update_number = entity.volatile_update_number.wrapping_sub(1);
        self.detail_level = entity.lod_levels;
        self.id_alias = NO_ID_ALIAS;
        // DBL_MAX marks "last delta was infinitely large" so the next delta is
        // not capped by the growth throttle.
        self.last_priority_delta = f64::MAX;
        let levels = self.detail_level as usize;
        self.lod_event_numbers[..levels].fill(0);
    }

    /// `onEntityRemovedFromClient` — clear the client flags (retaining server
    /// ones: WITHHELD, MANUAL/TRIGGER source, PRIORITISED) and resnap the
    /// client state to the entity.
    pub fn on_entity_removed_from_client(&mut self, entity: &EntitySnapshot) {
        self.flags &= !CLIENT_STATE;
        self.reset_client_state(entity);
    }

    /// `reuse` — the entity re-entered before it had been removed. Throw the
    /// cache back into sync, or take the REFRESH detour when the client's
    /// last event has been trimmed off the entity's history.
    pub fn reuse(&mut self, entity: &EntitySnapshot) {
        assert!(self.flags & GONE != 0);
        self.flags &= !GONE;

        if self.last_event_number < entity.last_trimmed_event_number
            && self.flags & NOT_UPDATABLE == 0
        {
            self.flags |= REFRESH;
        }
    }

    pub fn set_flag(&mut self, flag: u16) {
        self.flags |= flag;
    }

    pub fn clear_flag(&mut self, flag: u16) {
        self.flags &= !flag;
    }

    pub fn has_flag(&self, flag: u16) -> bool {
        self.flags & flag != 0
    }

    /// `isUpdatable` — no pending/dead/withheld state.
    pub fn is_updatable(&self) -> bool {
        self.flags & NOT_UPDATABLE == 0
    }

    pub fn priority(&self) -> f64 {
        self.priority
    }

    pub fn set_priority(&mut self, priority: f64) {
        self.priority = priority;
    }

    pub fn id_alias(&self) -> u8 {
        self.id_alias
    }

    pub fn set_id_alias(&mut self, id_alias: u8) {
        self.id_alias = id_alias;
    }

    pub fn vehicle_change_num(&self) -> u8 {
        self.vehicle_change_num
    }

    pub fn set_vehicle_change_num(&mut self, num: u8) {
        self.vehicle_change_num = num;
    }

    pub fn last_event_number(&self) -> i32 {
        self.last_event_number
    }

    /// `lastVolatileUpdateNumber`.
    pub fn last_volatile_update_number(&self) -> u16 {
        self.last_volatile_update_number
    }

    /// `updateSchemeID`.
    pub fn update_scheme_id(&self) -> u8 {
        self.update_scheme_id
    }

    pub fn detail_level(&self) -> u8 {
        self.detail_level
    }

    /// `lodEventNumber( detailLevel )` readback.
    pub fn lod_event_numbers(&self) -> [i32; MAX_LOD_LEVELS] {
        self.lod_event_numbers
    }

    /// Full flag word readback.
    pub fn flags(&self) -> u16 {
        self.flags
    }

    /// `lodEventNumbers` — set the top `stamps.len()` levels.
    pub fn set_lod_event_numbers(&mut self, entity: &EntitySnapshot, stamps: &[i32]) {
        let offset = (entity.lod_levels as usize).saturating_sub(stamps.len());
        for (i, stamp) in stamps.iter().enumerate() {
            self.lod_event_numbers[i + offset] = *stamp;
        }
    }

    /// `getDistanceSquared` — x/z only, clamped at FLT_MAX.
    fn distance_squared(&self, entity: &EntitySnapshot, origin: (f32, f32, f32)) -> f32 {
        let diff_x = entity.position.0 - origin.0;
        let diff_z = entity.position.2 - origin.2;
        let ret = diff_x * diff_x + diff_z * diff_z;
        if ret > f32::MAX { f32::MAX } else { ret }
    }

    /// `getLoDPriority`.
    pub fn lod_priority(&self, entity: &EntitySnapshot, origin: (f32, f32, f32)) -> f32 {
        self.distance_squared(entity, origin)
    }

    /// `updatePriority` — accumulate the scheme delta, capped to a fraction of
    /// the previous delta by the growth throttle.
    pub fn update_priority(&mut self, entity: &EntitySnapshot, origin: (f32, f32, f32)) {
        let dist_sq = self.distance_squared(entity, origin);
        let mut delta = scheme_for(entity.aoi_update_scheme_id).apply(dist_sq);
        delta = delta.min(WITNESS_UPDATE_DELTA_GROWTH_THROTTLE * self.last_priority_delta);
        self.last_priority_delta = delta;
        self.priority += delta;
    }

    /// `addOuterDetailLevel` — created one level below outermost, stamped
    /// with the entity's current event. The changed-properties body itself is
    /// BW-08-03-04.
    pub fn add_outer_detail_level(&mut self, entity: &EntitySnapshot) {
        assert_eq!(self.detail_level, entity.lod_levels);
        self.detail_level = entity.lod_levels - 1;
        self.last_event_number = entity.last_event_number;
    }

    /// `addLeaveAoIMessage` stamps — entered levels carry their stored
    /// numbers, the current and lower levels all sit at `lastEventNumber`.
    fn leave_aoi_stamps(&self, entity: &EntitySnapshot) -> Vec<i32> {
        let mut stamps: Vec<i32> = self.lod_event_numbers[..self.detail_level as usize].to_vec();
        for _ in self.detail_level as usize..entity.lod_levels as usize {
            stamps.push(self.last_event_number);
        }
        stamps
    }
}

/// Port of `Witness` — the AoI-management face. The owning entity's snapshot
/// lives in `entities` alongside everything it observes, mirroring how the
/// reference reads `entity_` directly and finds others via the CellApp.
#[derive(Debug)]
pub struct Witness {
    entity_id: EntityId,
    /// `CellApp::findEntity` stand-in: every entity this witness can see,
    /// including its own.
    entities: HashMap<EntityId, EntitySnapshot>,
    /// `aoiMap_`.
    aoi_map: HashMap<EntityId, EntityCache>,
    /// `entityQueue_` — the reference keeps a min-heap on priority; the
    /// 03-02 state-change pass only reads the minimum (in `addToSeen`), so a
    /// plain vector with swap-pop removal keeps the same behaviour. The real
    /// heap ordering arrives with BW-08-03-03.
    entity_queue: Vec<EntityId>,
    /// `freeAliases_` — LIFO pool; slot 0xff is reserved for `NO_ID_ALIAS`,
    /// so the filled pool is 0..254 ascending and allocation pops the back.
    free_aliases: Vec<u8>,
    /// `bundle()` stand-in — messages waiting to go to the client.
    bundle: Vec<ClientMessage>,
    /// WARNING_MSG / CRITICAL_MSG records.
    warnings: Vec<String>,
    /// `entity_.isInAoIOffload()` — set when an add was deferred for a dead
    /// entity during offload, cleared by the matching remove.
    is_in_aoi_offload: bool,
    aoi_radius: f32,
    aoi_hyst: f32,
    /// `maxPacketSize_` — payload bytes per tick, 0 until the proxy tells us
    /// the client's rate via `setWitnessCapacity`.
    max_packet_size: i32,
    /// `bandwidthDeficit_` — bytes we overshot last tick, paid back now.
    bandwidth_deficit: i32,
    /// `CellApp::emergencyThrottle()` snapshot for this tick.
    emergency_throttle: f32,
    /// Fixed encoded size charged per entity update (harness face for the
    /// 03-03 budget scenarios); `None` charges the computed payload size.
    update_bytes: Option<usize>,
    /// Running `Mercury::Bundle::size` for the budget gates.
    bundle_bytes: usize,
    /// `referencePosition_` — the grid-aligned point relative positions are
    /// measured from.
    reference_position: (f32, f32, f32),
    /// `hasReferencePosition_` — the client has supplied a reference.
    has_reference_position: bool,
    /// `referenceSeqNum_`.
    reference_seq_num: u32,
    /// `CellApp::instance().time()` stand-in; bumped at the end of each
    /// update pass. Starts at 1 so the "never sent" sentinel of
    /// `last_sent_reliable_game_time` stays distinct.
    time: u32,
    /// `CellAppConfig::maxTimeBetweenPlayerUpdatesInTicks()`.
    max_time_between_player_updates_in_ticks: i32,
    /// `lastSentReliableGameTime_` — 0 means "never".
    last_sent_reliable_game_time: u32,
    /// `lastSentReliablePosition_`.
    last_sent_reliable_position: (f32, f32, f32),
    /// `lastSentReliableDirection_`.
    last_sent_reliable_direction: (f32, f32, f32),
}

impl Witness {
    /// `Witness::Witness` for `CREATE_REAL_FROM_INIT`.
    pub fn new(entity_id: EntityId, self_entity: EntitySnapshot) -> Self {
        let reference_position = (
            self_entity.position.0.round(),
            self_entity.position.1.round(),
            self_entity.position.2.round(),
        );
        let mut entities = HashMap::new();
        entities.insert(entity_id, self_entity);
        Witness {
            entity_id,
            entities,
            aoi_map: HashMap::new(),
            entity_queue: Vec::new(),
            free_aliases: (0..=254u8).collect(),
            bundle: Vec::new(),
            warnings: Vec::new(),
            is_in_aoi_offload: false,
            aoi_radius: DEFAULT_AOI_RADIUS,
            aoi_hyst: 5.0,
            max_packet_size: 0,
            bandwidth_deficit: 0,
            emergency_throttle: 1.0,
            update_bytes: None,
            bundle_bytes: 0,
            reference_position,
            has_reference_position: false,
            reference_seq_num: 0,
            time: 1,
            max_time_between_player_updates_in_ticks: 0,
            last_sent_reliable_game_time: 0,
            last_sent_reliable_position: (0.0, 0.0, 0.0),
            last_sent_reliable_direction: (0.0, 0.0, 0.0),
        }
    }

    pub fn entity_id(&self) -> EntityId {
        self.entity_id
    }

    /// Harness face — refresh the authoritative copy of an entity
    /// (`Entity::position` etc. mutate in place upstream).
    pub fn upsert_entity(&mut self, entity: EntitySnapshot) {
        self.entities.insert(entity.id, entity);
    }

    pub fn entity(&self, id: EntityId) -> Option<&EntitySnapshot> {
        self.entities.get(&id)
    }

    /// `Witness::setAoIRadius` bookkeeping half; the trigger rewiring is the
    /// range list's business.
    pub fn set_aoi_radius(&mut self, radius: f32, hyst: f32) {
        self.aoi_radius = radius.max(0.1);
        self.aoi_hyst = hyst;
    }

    pub fn aoi_radius(&self) -> f32 {
        self.aoi_radius
    }

    pub fn aoi_hyst(&self) -> f32 {
        self.aoi_hyst
    }

    /// `Witness::setWitnessCapacity` — the proxy reports the client's rate;
    /// a zero hertz leaves the packet size untouched.
    pub fn set_witness_capacity(&mut self, bits_per_second: i32, update_hertz: i32) {
        // a zero hertz leaves the packet size untouched
        self.max_packet_size = if update_hertz != 0 {
            bps_to_packet_size(bits_per_second, update_hertz)
        } else {
            self.max_packet_size
        };
    }

    pub fn max_packet_size(&self) -> i32 {
        self.max_packet_size
    }

    /// `CellApp::emergencyThrottle()` stand-in — the harness pins it per tick.
    pub fn set_emergency_throttle(&mut self, throttle: f32) {
        self.emergency_throttle = throttle;
    }

    /// Harness face for the encoded update size — fixes the per-update
    /// budget charge; `clear_entity_update_bytes` returns to the computed
    /// payload sizes.
    pub fn set_entity_update_bytes(&mut self, bytes: usize) {
        self.update_bytes = Some(bytes);
    }

    pub fn clear_entity_update_bytes(&mut self) {
        self.update_bytes = None;
    }

    /// `CellAppConfig::maxTimeBetweenPlayerUpdatesInTicks` harness face.
    pub fn set_max_time_between_player_updates(&mut self, ticks: i32) {
        self.max_time_between_player_updates_in_ticks = ticks;
    }

    /// The client has supplied a reference position (`onSendReliablePosition`
    /// counterpart on the receiving side).
    pub fn set_client_reference_sequence(&mut self, sequence_number: u32) {
        self.has_reference_position = true;
        self.reference_seq_num = sequence_number;
    }

    /// `CellApp::instance().time()` readback.
    pub fn time(&self) -> u32 {
        self.time
    }

    /// `Mercury::Bundle::size` readback.
    pub fn bundle_size(&self) -> usize {
        self.bundle_bytes
    }

    pub fn bandwidth_deficit(&self) -> i32 {
        self.bandwidth_deficit
    }

    /// `bundle()` — inspect queued messages without consuming them.
    pub fn bundle(&self) -> &[ClientMessage] {
        &self.bundle
    }

    /// Send everything queued and clear the bundle.
    pub fn take_bundle(&mut self) -> Vec<ClientMessage> {
        self.bundle_bytes = 0;
        std::mem::take(&mut self.bundle)
    }

    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }

    /// Readback faces for tests.
    pub fn aoi_ids(&self) -> Vec<EntityId> {
        let mut ids: Vec<EntityId> = self.aoi_map.keys().copied().collect();
        ids.sort_unstable();
        ids
    }

    pub fn queue_ids(&self) -> Vec<EntityId> {
        self.entity_queue.clone()
    }

    pub fn cache_flags(&self, id: EntityId) -> Option<u16> {
        self.aoi_map.get(&id).map(|c| c.flags)
    }

    pub fn cache_priority(&self, id: EntityId) -> Option<f64> {
        self.aoi_map.get(&id).map(|c| c.priority)
    }

    pub fn cache_id_alias(&self, id: EntityId) -> Option<u8> {
        self.aoi_map.get(&id).map(|c| c.id_alias)
    }

    fn cache_mut(&mut self, id: EntityId) -> &mut EntityCache {
        self.aoi_map.get_mut(&id).expect("cache present in AoI map")
    }

    /// `Witness::addToAoI` — the entity came into range (or was added by
    /// script). Not actually processed until the state-change pass runs.
    pub fn add_to_aoi(&mut self, id: EntityId, manually_added: bool) {
        let source = if manually_added {
            MANUALLY_ADDED
        } else {
            ADDED_BY_TRIGGER
        };

        let Some(entity) = self.entities.get(&id).cloned() else {
            return;
        };

        if entity.destroyed {
            // `MF_ASSERT( !this->entity().isInAoIOffload() )`; the flag marks
            // that this dead entity was triggered artificially so the
            // matching removeFromAoI knows to be a no-op.
            assert!(!self.is_in_aoi_offload);
            self.is_in_aoi_offload = true;
            return;
        }

        if !entity.can_be_on_client {
            return;
        }

        if let Some(cache) = self.aoi_map.get_mut(&id) {
            if cache.has_flag(GONE) {
                cache.reuse(&entity);
            } else {
                cache.set_flag(source);
                return;
            }
        } else {
            self.aoi_map.insert(id, EntityCache::new(&entity));
            self.cache_mut(id).set_flag(ENTER_PENDING);
            self.add_to_seen(id);
        }

        self.cache_mut(id).set_flag(source);
    }

    /// `Witness::removeFromAoI` — matched against the add's source flag; the
    /// entity is marked GONE and the update pass does the actual deletion.
    pub fn remove_from_aoi(&mut self, id: EntityId, clear_manually_added: bool) {
        // A remove matched with an ignored add of a dead entity (offload).
        if self.is_in_aoi_offload {
            self.is_in_aoi_offload = false;
            return;
        }

        let Some(entity) = self.entities.get(&id).cloned() else {
            return;
        };

        if !entity.can_be_on_client {
            return;
        }

        let Some(cache) = self.aoi_map.get_mut(&id) else {
            self.warnings.push(format!(
                "removeFromAoI: entity {id} not in AoI of entity {}",
                self.entity_id
            ));
            return;
        };

        if clear_manually_added {
            cache.clear_flag(MANUALLY_ADDED);
            if cache.has_flag(ADDED_BY_TRIGGER) {
                return;
            }
        } else {
            cache.clear_flag(ADDED_BY_TRIGGER);
            if cache.has_flag(MANUALLY_ADDED) {
                return;
            }
        }

        cache.set_flag(GONE);
    }

    /// `Witness::addToSeen` — join the queue at the minimum current priority
    /// so a newly seen entity is not starved by old neighbours' debt.
    pub fn add_to_seen(&mut self, id: EntityId) {
        let initial_priority = if self.entity_queue.is_empty() {
            0.0
        } else {
            self.entity_queue
                .iter()
                .filter_map(|qid| self.aoi_map.get(qid).map(|c| c.priority()))
                .fold(f64::INFINITY, f64::min)
        };

        self.cache_mut(id).set_priority(initial_priority);
        self.entity_queue.push(id);
    }

    /// `Witness::requestEntityUpdate` — the client replied to our enterAoI;
    /// stamp its LoD event numbers and put the cache back in the queue with
    /// CREATE_PENDING.
    pub fn request_entity_update(&mut self, id: EntityId, lod_stamps: &[i32]) {
        if !self.aoi_map.contains_key(&id) {
            if id != self.entity_id {
                self.warnings.push(format!(
                    "requestEntityUpdate: entity {id} is no longer in AoI"
                ));
            } else {
                self.warnings
                    .push(format!("requestEntityUpdate: entity {id} requested itself"));
            }
            return;
        }

        if !self.cache_mut(id).has_flag(REQUEST_PENDING) {
            self.warnings.push(format!(
                "requestEntityUpdate: request for {id} that is not pending"
            ));
            return;
        }

        self.cache_mut(id).clear_flag(REQUEST_PENDING);
        self.cache_mut(id).set_flag(CREATE_PENDING);

        let entity = self
            .entities
            .get(&id)
            .cloned()
            .expect("requestEntityUpdate on a known entity");
        if lod_stamps.len() > entity.lod_levels as usize {
            self.warnings
                .push("requestEntityUpdate: LoD stamp cheat".to_string());
        }
        self.cache_mut(id)
            .set_lod_event_numbers(&entity, lod_stamps);

        self.add_to_seen(id);
    }

    /// Script face for `withholdFromClient` — keep the entity from the client
    /// while it stays in the AoI.
    pub fn set_withheld(&mut self, id: EntityId) {
        self.cache_mut(id).set_flag(WITHHELD);
    }

    pub fn clear_withheld(&mut self, id: EntityId) {
        self.cache_mut(id).clear_flag(WITHHELD);
    }

    /// `Witness::allocateIDAlias` — only volatile entities get an alias.
    fn allocate_id_alias(&mut self, entity: &EntitySnapshot) -> u8 {
        if entity.has_volatile() && !self.free_aliases.is_empty() {
            return self.free_aliases.pop().expect("checked non-empty");
        }
        NO_ID_ALIAS
    }

    /// Queue a client message, keeping the budget's byte count in step with
    /// `Mercury::Bundle::size`.
    fn push_message(&mut self, message: ClientMessage) {
        self.bundle_bytes += message.wire_size();
        self.bundle.push(message);
    }

    /// `Witness::sendEnter` — hand out the alias, park the cache on
    /// REQUEST_PENDING and address the client.
    fn send_enter(&mut self, id: EntityId) {
        let entity = self.entities.get(&id).cloned().expect("entity in AoI");
        let alias = self.allocate_id_alias(&entity);

        let cache = self.cache_mut(id);
        assert!(!cache.has_flag(REQUEST_PENDING));
        cache.set_id_alias(alias);
        cache.clear_flag(ENTER_PENDING);
        cache.set_flag(REQUEST_PENDING);
        cache.set_vehicle_change_num(entity.vehicle_change_num);

        match entity.vehicle_id {
            Some(vehicle_id) => self.push_message(ClientMessage::EnterAoIOnVehicle {
                id,
                vehicle_id,
                id_alias: alias,
            }),
            None => self.push_message(ClientMessage::EnterAoI {
                id,
                id_alias: alias,
            }),
        }
    }

    /// `Witness::sendCreate` — the client asked for the full entity.
    fn send_create(&mut self, id: EntityId) {
        self.cache_mut(id).clear_flag(CREATE_PENDING);

        if self.cache_mut(id).has_flag(REFRESH) {
            self.warnings
                .push("sendCreate: isRefresh = true".to_string());
            self.cache_mut(id).clear_flag(REFRESH);
        }

        assert!(self.cache_mut(id).is_updatable());

        let entity = self.entities.get(&id).cloned().expect("entity in AoI");

        // `writeVehicleChangeToBundle` — tell the client if our vehicular
        // record is stale.
        let cached_num = self.cache_mut(id).vehicle_change_num();
        if cached_num != entity.vehicle_change_num {
            self.push_message(ClientMessage::SetVehicle {
                passenger_id: id,
                vehicle_id: entity.vehicle_id,
            });
            self.cache_mut(id)
                .set_vehicle_change_num(entity.vehicle_change_num);
        }

        let (yaw, pitch, roll) = (entity.yaw, entity.pitch, entity.roll);
        let message = if entity.has_volatile() {
            ClientMessage::CreateEntity {
                id,
                client_type_id: entity.client_type_id,
                position: entity.position,
                yaw,
                pitch,
                roll,
            }
        } else {
            ClientMessage::CreateEntityDetailed {
                id,
                client_type_id: entity.client_type_id,
                position: entity.position,
                yaw,
                pitch,
                roll,
            }
        };
        self.push_message(message);

        // `addOuterDetailLevel` — created one level below outermost.
        self.cache_mut(id).add_outer_detail_level(&entity);
    }

    /// `Witness::deleteFromClient` — the client stops knowing this entity.
    /// WITHHELD survives the reset.
    fn delete_from_client(&mut self, id: EntityId) {
        self.cache_mut(id).clear_flag(REFRESH);

        let entity = self
            .entities
            .get(&id)
            .cloned()
            .expect("deleteFromClient on a known entity");

        if !self.cache_mut(id).has_flag(ENTER_PENDING) {
            let stamps = self.cache_mut(id).leave_aoi_stamps(&entity);
            self.push_message(ClientMessage::LeaveAoI {
                id,
                lod_event_numbers: stamps,
            });
        }

        self.on_leave_aoi(id);

        // Reset client related state.
        self.cache_mut(id).on_entity_removed_from_client(&entity);
    }

    /// `Witness::onLeaveAoI` — return the alias to the pool.
    fn on_leave_aoi(&mut self, id: EntityId) {
        let alias = self.cache_mut(id).id_alias();
        if alias != NO_ID_ALIAS {
            self.free_aliases.push(alias);
        }
    }

    /// `Witness::handleStateChange` — dispatch one cache's pending flags.
    /// Returns true when the entry leaves the queue: deletions and enter
    /// sends take it out (deletions drop the cache from the map too), while
    /// withheld and create-pending caches stay queued.
    fn apply_state_change(&mut self, id: EntityId, origin: (f32, f32, f32)) -> bool {
        assert!(!self.cache_mut(id).has_flag(REQUEST_PENDING));

        if self.cache_mut(id).has_flag(GONE) {
            self.delete_from_client(id);
            self.aoi_map.remove(&id);
            true
        } else if self.cache_mut(id).has_flag(WITHHELD) {
            // If the client knows about a withheld entity, make it "un-know".
            if !self.cache_mut(id).has_flag(ENTER_PENDING) {
                self.delete_from_client(id);
                // The WITHHELD flag survives deleteFromClient's reset.
                self.cache_mut(id).set_flag(ENTER_PENDING);
            }
            // Leave withheld entities queued.
            false
        } else if self.cache_mut(id).has_flag(ENTER_PENDING) {
            self.send_enter(id);
            // Enter-pending caches rejoin the queue via requestEntityUpdate.
            true
        } else if self.cache_mut(id).has_flag(CREATE_PENDING) {
            self.send_create(id);
            let entity = self.entities.get(&id).cloned().expect("entity in AoI");
            self.cache_mut(id).update_priority(&entity, origin);
            false
        } else if self.cache_mut(id).has_flag(REFRESH) {
            self.cache_mut(id).clear_flag(REFRESH);
            // We are effectively resetting this cache's history: tell the
            // client it is gone and (re-)entered.
            self.delete_from_client(id);
            self.cache_mut(id).set_flag(ENTER_PENDING);
            self.send_enter(id);
            true
        } else {
            unreachable!("queued cache with no pending state");
        }
    }

    /// The queue's heap front: minimum priority, ties broken by entity id so
    /// the send sequence stays deterministic. The reference's std heap leaves
    /// ties unspecified.
    fn queue_front(&self) -> Option<EntityId> {
        self.entity_queue.iter().copied().min_by_key(|id| {
            let prio = self.aoi_map.get(id).map(|c| c.priority());
            (prio.unwrap_or(f64::INFINITY).to_bits(), *id)
        })
    }

    /// `std::pop_heap` — take the front out of the queue.
    fn queue_pop(&mut self) -> Option<EntityId> {
        let front = self.queue_front()?;
        let index = self
            .entity_queue
            .iter()
            .position(|id| *id == front)
            .expect("front still queued");
        self.entity_queue.swap_remove(index);
        Some(front)
    }

    /// `std::push_heap` / `std::make_heap` — (re)join the queue.
    fn queue_push(&mut self, id: EntityId) {
        self.entity_queue.push(id);
    }

    /// `EntityCache::getLoDPriority` — squared x/z distance to the origin,
    /// or 0 for coincident schemes.
    fn lod_priority_of(entity: &EntitySnapshot, origin: (f32, f32, f32)) -> f32 {
        if scheme_for(entity.aoi_update_scheme_id).is_coincident() {
            return 0.0;
        }
        let dx = entity.position.0 - origin.0;
        let dz = entity.position.2 - origin.2;
        let ret = dx * dx + dz * dz;
        // FLT_MAX squared results in +inf; fix that
        if ret > f32::MAX { f32::MAX } else { ret }
    }

    /// `PackedXZ::maxLimit` — `getLimit<3, 8>` works out to 510 at scale 1.
    fn packed_limit(&self) -> f32 {
        510.0 * (500.0 / 510.0)
    }

    /// `Witness::sendQueueElement` — one entity update: the vehicle change,
    /// the LoD-gated event history and the volatile position/direction
    /// projection, all on this tick's budget. Returns whether a reliable
    /// relative position went out (reliability itself is not modelled).
    fn send_queue_element(&mut self, id: EntityId) -> bool {
        let entity = self.entities.get(&id).cloned().expect("entity in AoI");
        let origin = self
            .entities
            .get(&self.entity_id)
            .map(|e| e.position)
            .expect("witness's own entity registered");
        let lod_priority = Witness::lod_priority_of(&entity, origin);

        // `writeVehicleChangeToBundle` — the client's record of this
        // entity's vehicular state must match before anything else rides.
        let vehicle_changed = self
            .aoi_map
            .get(&id)
            .map(|cache| cache.vehicle_change_num != entity.vehicle_change_num)
            .unwrap_or(false);
        if vehicle_changed {
            self.push_message(ClientMessage::SetVehicle {
                passenger_id: id,
                vehicle_id: entity.vehicle_id,
            });
            self.aoi_map
                .get_mut(&id)
                .expect("queued cache present")
                .vehicle_change_num = entity.vehicle_change_num;
        }

        // Which pending history events clear the LoD gate?
        let (last_event_number, detail_level, id_alias) = {
            let cache = self.aoi_map.get(&id).expect("queued cache present");
            (cache.last_event_number, cache.detail_level, cache.id_alias)
        };
        let pending: Vec<HistoryEvent> = entity
            .events
            .iter()
            .filter(|e| e.number > last_event_number)
            .copied()
            .collect();
        let has_events_to_send = pending
            .iter()
            .any(|e| e.should_send(lod_priority, detail_level));

        // The volatile projection. `cache.lastVolatileUpdateNumber` stamps
        // before the gate so unchanged positions are not resent.
        let mut pos = PosProjection::None;
        let mut dir = DirProjection::None;
        let mut has_selected_entity = false;
        {
            let cache = self.aoi_map.get_mut(&id).expect("queued cache present");
            if cache.last_volatile_update_number != entity.volatile_update_number {
                cache.last_volatile_update_number = entity.volatile_update_number;
                if entity.volatile.has_volatile(lod_priority) {
                    let always_detailed = cache.has_flag(IS_ALWAYS_DETAILED)
                        || (cache.has_flag(PRIORITISED) && SEND_DETAILED_PLAYER_VEHICLES);
                    let dir_type = entity.volatile.dir_type(lod_priority);
                    dir = match dir_type {
                        0 => DirProjection::YawPitchRoll {
                            yaw: entity.yaw,
                            pitch: entity.pitch,
                            roll: entity.roll,
                        },
                        1 => DirProjection::YawPitch {
                            yaw: entity.yaw,
                            pitch: entity.pitch,
                        },
                        2 => DirProjection::Yaw { yaw: entity.yaw },
                        _ => DirProjection::None,
                    };
                    if !entity.volatile.should_send_position() {
                        pos = PosProjection::None;
                    } else if always_detailed {
                        pos = PosProjection::Detailed {
                            x: entity.position.0,
                            y: entity.position.1,
                            z: entity.position.2,
                        };
                    } else {
                        // A passenger's packed position stays absolute; a
                        // ground entity's is relative to our reference.
                        let is_relative = entity.vehicle_id.is_none();
                        let raw = if is_relative {
                            (
                                entity.position.0 - self.reference_position.0,
                                entity.position.1 - self.reference_position.1,
                                entity.position.2 - self.reference_position.2,
                            )
                        } else {
                            entity.position
                        };
                        let limit = self.packed_limit();
                        if raw.0 < -limit || limit <= raw.0 || raw.2 < -limit || limit <= raw.2 {
                            pos = PosProjection::Detailed {
                                x: entity.position.0,
                                y: entity.position.1,
                                z: entity.position.2,
                            };
                        } else {
                            let (x, y, z) = raw;
                            pos = if is_relative {
                                PosProjection::Relative {
                                    x,
                                    y,
                                    z,
                                    on_ground: entity.is_on_ground,
                                }
                            } else {
                                PosProjection::Absolute {
                                    x,
                                    y,
                                    z,
                                    on_ground: entity.is_on_ground,
                                }
                            };
                        }
                    }
                    has_selected_entity = true;
                }
            }
        }

        // History events ride in chronological order behind a select.
        let mut events = Vec::new();
        if has_events_to_send {
            if !has_selected_entity {
                if id_alias != NO_ID_ALIAS {
                    self.push_message(ClientMessage::SelectAliasedEntity { id_alias });
                } else {
                    self.push_message(ClientMessage::SelectEntity { id });
                }
                has_selected_entity = true;
            }
            events = pending
                .iter()
                .filter(|e| e.should_send(lod_priority, detail_level))
                .map(|e| e.number)
                .collect();
        }

        // `cache.lastEventNumber( this->lastEventNumber() )` stamps even
        // when nothing was eligible.
        self.aoi_map
            .get_mut(&id)
            .expect("queued cache present")
            .last_event_number = entity.last_event_number;

        // `EntityCache::updateDetailLevel` — detail transitions stamp their
        // LoD levels on the way out and resend properties on the way in.
        let mut props_level = None;
        {
            let cache = self.aoi_map.get_mut(&id).expect("queued cache present");
            // An unconfigured level has no transition point, so neither
            // transition fires.
            while cache.detail_level > 0
                && lod_priority
                    < entity
                        .lod_thresholds
                        .get(cache.detail_level as usize)
                        .map(|t| t.low)
                        .unwrap_or(-1.0)
            {
                cache.detail_level -= 1;
                props_level = Some(cache.detail_level);
            }
            while (cache.detail_level as usize) < entity.lod_thresholds.len()
                && lod_priority
                    > entity
                        .lod_thresholds
                        .get(cache.detail_level as usize)
                        .map(|t| t.high)
                        .unwrap_or(-1.0)
            {
                cache.lod_event_numbers[cache.detail_level as usize] = cache.last_event_number;
                cache.detail_level += 1;
            }
        }
        let bytes = self.update_bytes.unwrap_or_else(|| {
            let mut bytes = if id_alias != NO_ID_ALIAS { 1 } else { 4 };
            bytes += match pos {
                PosProjection::Relative {
                    on_ground: true, ..
                }
                | PosProjection::Absolute {
                    on_ground: true, ..
                } => 3, // PackedXZ
                PosProjection::Relative { .. } | PosProjection::Absolute { .. } => 5, // PackedXYZ
                PosProjection::Detailed { .. } => 12,
                PosProjection::None => 0,
            };
            bytes += match dir {
                DirProjection::YawPitchRoll { .. } => 12,
                DirProjection::YawPitch { .. } => 8,
                DirProjection::Yaw { .. } => 4,
                DirProjection::None => 0,
            };
            bytes += pending
                .iter()
                .filter(|e| e.should_send(lod_priority, detail_level))
                .map(|e| 4 + e.payload_bytes)
                .sum::<usize>();
            bytes
        });
        self.push_message(ClientMessage::EntityUpdate {
            id,
            id_alias: (id_alias != NO_ID_ALIAS).then_some(id_alias),
            pos,
            dir,
            events,
            bytes,
        });

        // `updateDetailLevel` appends its select + changed-properties resend
        // after the rest of the entity's stream.
        if let Some(level) = props_level {
            if !has_selected_entity {
                if id_alias != NO_ID_ALIAS {
                    self.push_message(ClientMessage::SelectAliasedEntity { id_alias });
                } else {
                    self.push_message(ClientMessage::SelectEntity { id });
                }
            }
            self.push_message(ClientMessage::UpdateProperties {
                id,
                id_alias: (id_alias != NO_ID_ALIAS).then_some(id_alias),
                detail_level: level,
            });
        }
        false
    }

    /// Returns and clears the PRIORITISED marker of a queued cache.
    fn drop_prioritised_flag(&mut self, id: &EntityId) -> bool {
        match self.aoi_map.get_mut(id) {
            Some(cache) if cache.has_flag(PRIORITISED) => {
                cache.clear_flag(PRIORITISED);
                true
            }
            _ => false,
        }
    }

    /// A prioritised vehicle that also popped inside the window restarts
    /// from the tick's starting priority so it is not starved behind its own
    /// pre-send.
    fn restart_prioritised_from(
        &mut self,
        id: EntityId,
        starting_priority: f64,
        origin: (f32, f32, f32),
    ) {
        let cache = self
            .aoi_map
            .get_mut(&id)
            .expect("prioritised cache present");
        cache.set_priority(starting_priority);
        cache.clear_flag(PRIORITISED);
        let entity = self.entities.get(&id).cloned().expect("entity in AoI");
        cache.update_priority(&entity, origin);
    }

    /// `Witness::addReferencePosition` — makes sure the bundle carries a
    /// reference position, either the client's own sequence or a fresh
    /// grid-aligned one. Returns whether a message was added.
    fn add_reference_position(&mut self) -> bool {
        let self_entity = self
            .entities
            .get(&self.entity_id)
            .cloned()
            .expect("witness's own entity registered");

        // Fast path: client-controlled witness which has sent us a
        // reference position at some point.
        if self_entity.controlled_by_self && self.has_reference_position {
            self.push_message(ClientMessage::RelativePositionReference {
                sequence_number: self.reference_seq_num,
            });
            return true;
        }

        // Clear out any now-useless client-supplied reference position.
        self.has_reference_position = false;

        // The witness is either server-controlled, or newly client-controlled
        // and hasn't sent us a reference position to use. So we choose one.
        self.reference_position = (
            self_entity.position.0.round(),
            self_entity.position.1.round(),
            self_entity.position.2.round(),
        );

        if self_entity.vehicle_id.is_some() || self_entity.controlled_by_self {
            self.push_message(ClientMessage::RelativePosition {
                position: self.reference_position,
            });
            return true;
        }
        false
    }

    /// `Witness::addDetailedPlayerPosition` — the witness's own projection,
    /// ahead of the observed entities. Skipped for non-volatile or
    /// client-controlled entities, and for unchanged positions inside the
    /// resend window.
    fn add_detailed_player_position(&mut self, is_reference_position: bool) {
        let entity = self
            .entities
            .get(&self.entity_id)
            .cloned()
            .expect("witness's own entity registered");

        // Non-volatile or witness-controlled entity: don't send the position.
        if !entity.volatile.has_volatile(0.0) || entity.controlled_by_self {
            return;
        }

        // Check that we're allowed to skip an unchanged player position
        // update.
        let has_sent_player_position = self.last_sent_reliable_game_time != 0;
        let always_skip_unchanged = self.max_time_between_player_updates_in_ticks == -1;
        let time_since_last_send = self.time.wrapping_sub(self.last_sent_reliable_game_time);
        let last_send_was_recent_enough = always_skip_unchanged
            || time_since_last_send <= self.max_time_between_player_updates_in_ticks as u32;
        let direction = (entity.yaw, entity.pitch, entity.roll);
        let should_skip_unchanged_player_position = has_sent_player_position
            && last_send_was_recent_enough
            && entity.position == self.last_sent_reliable_position
            && direction == self.last_sent_reliable_direction;

        if !is_reference_position && should_skip_unchanged_player_position {
            return;
        }

        self.push_message(ClientMessage::AvatarUpdatePlayerDetailed {
            position: entity.position,
            yaw: entity.yaw,
            pitch: entity.pitch,
            roll: entity.roll,
        });

        // The message is reliable (and so stamped) unless it is being used
        // as the reference position.
        let reliable = !is_reference_position && self.max_time_between_player_updates_in_ticks != 0;
        if reliable {
            self.last_sent_reliable_game_time = self.time;
            self.last_sent_reliable_position = entity.position;
            self.last_sent_reliable_direction = direction;
        }
    }

    /// `Witness::update` — the per-tick client update pass. Sends the vehicle
    /// stack first, then walks the priority queue under the packet budget:
    /// entries within `MAX_PRIORITY_DELTA` of the front's priority pop and
    /// update until the budget or the window runs out. Deletions and
    /// enter/create handshakes ride the same pass.
    pub fn update(&mut self) {
        let origin = self
            .entities
            .get(&self.entity_id)
            .map(|e| e.position)
            .expect("witness's own entity registered");

        let desired_packet_size = (self.max_packet_size as f32 * self.emergency_throttle) as i32
            - self.bandwidth_deficit
            + self.bundle_bytes as i32;

        // A reference position rides first (the client's own sequence, or a
        // fresh grid-aligned one); the player's detailed projection doubles
        // as the reference when no separate message went out.
        let pushed_reference = self.add_reference_position();
        let use_player_as_reference = !pushed_reference;
        self.add_detailed_player_position(use_player_as_reference);

        let mut num_prioritised = 0usize;

        // Send the vehicle stack first, ahead of priority order.
        let mut vehicle = self
            .entities
            .get(&self.entity_id)
            .and_then(|e| e.vehicle_id);
        while let Some(id) = vehicle {
            let Some(cache) = self.aoi_map.get_mut(&id) else {
                self.warnings.push(format!(
                    "Witness::update: witness {} trying to prioritise entity {id} outside its AoI",
                    self.entity_id
                ));
                break;
            };
            let entity_destroyed = self.entities.get(&id).map(|e| e.destroyed);
            if !cache.is_updatable()
                || cache.has_flag(REQUEST_PENDING)
                || entity_destroyed == Some(true)
            {
                vehicle = self.entities.get(&id).and_then(|e| e.vehicle_id);
                continue;
            }
            assert!(!cache.has_flag(PRIORITISED));
            cache.set_flag(PRIORITISED);
            num_prioritised += 1;
            self.send_queue_element(id);
            vehicle = self.entities.get(&id).and_then(|e| e.vehicle_id);
        }

        let starting_priority = self
            .queue_front()
            .and_then(|id| self.aoi_map.get(&id))
            .map(|c| c.priority())
            .unwrap_or(0.0);

        // Entities at a distance are never sent at full rate: only the
        // window [front, front + MAX_PRIORITY_DELTA) is walked this tick.
        let max_priority = starting_priority + MAX_PRIORITY_DELTA;

        // Popped entries, in pop order — [queueEnd, entityQueue_.end()) in
        // the reference.
        let mut popped: Vec<(EntityId, bool)> = Vec::new();

        while let Some(id) = self.queue_front() {
            let front_priority = self.aoi_map.get(&id).map(|c| c.priority()).unwrap_or(0.0);
            if front_priority >= max_priority || self.bundle_bytes as i32 >= desired_packet_size - 2
            {
                break;
            }
            self.queue_pop();
            let was_prioritised = self
                .aoi_map
                .get(&id)
                .map(|c| c.has_flag(PRIORITISED))
                .unwrap_or(false);

            // A destroyed entity still held open by a manual add drops out
            // here rather than being sent.
            let destroyed = self.entities.get(&id).map(|e| e.destroyed);
            let gone = self.aoi_map.get(&id).map(|c| c.has_flag(GONE));
            if destroyed == Some(true) && gone == Some(false) {
                assert!(
                    self.aoi_map
                        .get(&id)
                        .map(|c| c.has_flag(MANUALLY_ADDED))
                        .unwrap_or(false)
                );
                self.remove_from_aoi(id, true);
            }

            let cache = self
                .aoi_map
                .get(&id)
                .expect("queued cache present in AoI map");
            let survives = if !cache.is_updatable() {
                !self.apply_state_change(id, origin)
            } else {
                if !was_prioritised {
                    self.send_queue_element(id);
                    let entity = self.entities.get(&id).cloned().expect("entity in AoI");
                    self.cache_mut(id).update_priority(&entity, origin);
                }
                true
            };

            // Prioritised (vehicle) entries were sent before the loop; they
            // only restart from the tick's starting priority here.
            if was_prioritised {
                self.restart_prioritised_from(id, starting_priority, origin);
                num_prioritised -= 1;
            }

            if survives {
                popped.push((id, was_prioritised));
            }
        }

        // Pay back any budget overshoot next tick.
        self.bandwidth_deficit = (self.bundle_bytes as i32 - desired_packet_size).max(0);
        if self.bandwidth_deficit > self.max_packet_size {
            self.warnings.push(format!(
                "Witness::update: witness {} has a bandwidth deficit of {} bytes",
                self.entity_id, self.bandwidth_deficit
            ));
            self.bandwidth_deficit = self.max_packet_size;
        }

        if self.entity_queue.is_empty() && num_prioritised == 0 {
            // We sent everything: next tick starts everyone from zero, in
            // order of closeness. The popped entries stay queued (the
            // reference make-heaps the whole vector here).
            for (id, _) in &popped {
                let cache = self
                    .aoi_map
                    .get_mut(id)
                    .expect("requeued cache present in AoI map");
                cache.set_priority(0.0);
                self.queue_push(*id);
            }
        } else {
            let earliest_priority = self
                .queue_front()
                .and_then(|id| self.aoi_map.get(&id))
                .map(|c| c.priority())
                .unwrap_or(0.0);

            // Vehicles that stayed queued had their update go out before the
            // loop; drop the marker so they are prioritised once.
            let mut vehicle = self
                .entities
                .get(&self.entity_id)
                .and_then(|e| e.vehicle_id);
            while let Some(id) = vehicle {
                num_prioritised -= self.drop_prioritised_flag(&id) as usize;
                vehicle = self.entities.get(&id).and_then(|e| e.vehicle_id);
            }
            debug_assert_eq!(num_prioritised, 0);

            // Push the sent entries back, never "in the past": an entity that
            // should have gone out twice in one tick waits for the next one.
            for (id, _) in popped {
                if let Some(cache) = self
                    .aoi_map
                    .get_mut(&id)
                    .filter(|cache| cache.priority() < earliest_priority)
                {
                    cache.set_priority(earliest_priority);
                }
                self.queue_push(id);
            }
        }

        // `CellApp::instance().time()` advances between update passes.
        self.time = self.time.wrapping_add(1);
    }

    /// One pass over the queue's current contents, exactly like the reference
    /// loop's pop-heap sweep: each slot is visited once, deletions swap-pop
    /// the vacated slot so its new occupant is handled in the same pass.
    /// Updatable entries are left queued — the bandwidth/priority selection
    /// that sends their updates is BW-08-03-03.
    pub fn flush_state_changes(&mut self) {
        let origin = self
            .entities
            .get(&self.entity_id)
            .map(|e| e.position)
            .expect("witness's own entity registered");

        let mut i = 0;
        while i < self.entity_queue.len() {
            let id = self.entity_queue[i];
            let cache = self
                .aoi_map
                .get(&id)
                .expect("queued cache present in AoI map");
            if !cache.is_updatable() && self.apply_state_change(id, origin) {
                // the reference swap-pops the vacated slot
                self.entity_queue.swap_remove(i);
            } else {
                i += 1;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Harness: one witness entity plus whatever it observes.
    struct WitnessWorld {
        witness: Witness,
    }

    impl WitnessWorld {
        fn new() -> Self {
            let mut self_entity = EntitySnapshot::new(1, 1, (0.0, 0.0, 0.0));
            // The fixtures watch other entities; the self projection has its
            // own tests below.
            self_entity.volatile = VolatileInfo::NONE;
            let mut witness = Witness::new(1, self_entity);
            witness.upsert_entity(EntitySnapshot::new(2, 2, (10.0, 0.0, 0.0)));
            WitnessWorld { witness }
        }

        /// Full first-observation cycle: enterAoI, client request, create.
        fn enter(&mut self, id: EntityId) {
            self.witness.add_to_aoi(id, false);
            self.witness.flush_state_changes();
            assert!(matches!(
                self.witness.bundle.first(),
                Some(ClientMessage::EnterAoI { .. })
            ));
            self.witness.request_entity_update(id, &[]);
            self.witness.flush_state_changes();
        }

        fn leave(&mut self, id: EntityId) {
            self.witness.remove_from_aoi(id, false);
            self.witness.flush_state_changes();
        }

        fn take(&mut self) -> Vec<ClientMessage> {
            self.witness.take_bundle()
        }
    }

    #[test]
    fn first_observation_and_reentry_produce_identical_messages() {
        let mut world = WitnessWorld::new();

        world.enter(2);
        let first_cycle = world.take();
        assert_eq!(
            first_cycle,
            vec![
                ClientMessage::EnterAoI {
                    id: 2,
                    id_alias: 254
                },
                ClientMessage::CreateEntity {
                    id: 2,
                    client_type_id: 2,
                    position: (10.0, 0.0, 0.0),
                    yaw: 0.0,
                    pitch: 0.0,
                    roll: 0.0,
                },
            ]
        );

        world.leave(2);
        assert_eq!(
            world.take(),
            vec![ClientMessage::LeaveAoI {
                id: 2,
                lod_event_numbers: vec![0],
            }]
        );

        // The alias went back to the pool, so the re-entry hands out the same
        // one and the client sees the identical enter/create pair.
        world.enter(2);
        let second_cycle = world.take();
        assert_eq!(first_cycle, second_cycle);
    }

    #[test]
    fn reenter_before_the_deletion_pass_reuses_the_cache() {
        let mut world = WitnessWorld::new();

        world.enter(2);
        world.take();
        assert!(world.witness.aoi_ids().contains(&2));

        // Leave then re-enter before the pass that would have deleted the
        // cache: reuse() throws it back into sync and the client, which
        // already knows the entity, is told nothing.
        world.witness.remove_from_aoi(2, false);
        assert_eq!(world.witness.cache_flags(2), Some(GONE));

        world.witness.add_to_aoi(2, false);
        assert_eq!(world.witness.cache_flags(2), Some(ADDED_BY_TRIGGER));
        world.witness.flush_state_changes();
        assert!(world.witness.bundle().is_empty());
        assert!(world.witness.aoi_ids().contains(&2));
    }

    #[test]
    fn a_cache_missing_trimmed_events_refreshes_through_leave_and_enter() {
        let mut world = WitnessWorld::new();

        world.enter(2);
        world.take();

        // The entity's history moved on and the early events were trimmed
        // past what the client last received.
        let mut entity = world.witness.entity(2).cloned().unwrap();
        entity.last_event_number = 10;
        entity.last_trimmed_event_number = 5;
        world.witness.upsert_entity(entity);

        world.witness.remove_from_aoi(2, false);
        world.witness.add_to_aoi(2, false);

        // reuse() could not restore sync, so the cache takes the REFRESH
        // detour: the client is told the entity left and re-entered at once.
        world.witness.flush_state_changes();
        assert_eq!(
            world.take(),
            vec![
                ClientMessage::LeaveAoI {
                    id: 2,
                    lod_event_numbers: vec![0],
                },
                ClientMessage::EnterAoI {
                    id: 2,
                    id_alias: 254,
                },
            ]
        );
    }

    #[test]
    fn withheld_known_entity_is_deleted_then_reenters_when_released() {
        let mut world = WitnessWorld::new();

        world.enter(2);
        world.take();

        world.witness.set_withheld(2);
        world.witness.flush_state_changes();
        assert_eq!(
            world.take(),
            vec![ClientMessage::LeaveAoI {
                id: 2,
                lod_event_numbers: vec![0],
            }]
        );
        // WITHHELD survives the client-state reset (as does the server-side
        // source flag) and the cache waits in enter-pending.
        assert_eq!(
            world.witness.cache_flags(2),
            Some(ADDED_BY_TRIGGER | WITHHELD | ENTER_PENDING)
        );

        // Still withheld: the pass leaves it queued and silent.
        world.witness.flush_state_changes();
        assert!(world.witness.bundle().is_empty());

        world.witness.clear_withheld(2);
        world.witness.flush_state_changes();
        assert_eq!(
            world.take(),
            vec![ClientMessage::EnterAoI {
                id: 2,
                id_alias: 254,
            }]
        );
    }

    #[test]
    fn manual_and_trigger_adds_pair_their_removes() {
        let mut world = WitnessWorld::new();

        world.witness.add_to_aoi(2, true);
        world.witness.add_to_aoi(2, false);
        assert_eq!(
            world.witness.cache_flags(2),
            Some(MANUALLY_ADDED | ADDED_BY_TRIGGER | ENTER_PENDING)
        );

        // The trigger drop leaves the manual add holding the entity in AoI.
        world.witness.remove_from_aoi(2, false);
        assert_eq!(
            world.witness.cache_flags(2),
            Some(MANUALLY_ADDED | ENTER_PENDING)
        );

        // Dropping the manual add finally marks it GONE (on top of the
        // never-serviced enter-pending bit). The client was never told about
        // it, so the deletion pass sends nothing at all.
        world.witness.remove_from_aoi(2, true);
        assert_eq!(world.witness.cache_flags(2), Some(ENTER_PENDING | GONE));
        world.witness.flush_state_changes();
        assert!(world.witness.bundle().is_empty());
        assert!(!world.witness.aoi_ids().contains(&2));

        // Same dance for entity 3 in the other order: dropping the manual
        // add first leaves the trigger holding the entity in AoI.
        world
            .witness
            .upsert_entity(EntitySnapshot::new(3, 2, (20.0, 0.0, 0.0)));
        world.witness.add_to_aoi(3, true);
        world.witness.add_to_aoi(3, false);
        world.witness.remove_from_aoi(3, true);
        assert_eq!(
            world.witness.cache_flags(3),
            Some(ADDED_BY_TRIGGER | ENTER_PENDING)
        );
        world.witness.remove_from_aoi(3, false);
        assert_eq!(world.witness.cache_flags(3), Some(ENTER_PENDING | GONE));
    }

    #[test]
    fn aliases_skip_the_reserved_value_and_recycle_lifo() {
        let mut world = WitnessWorld::new();
        world
            .witness
            .upsert_entity(EntitySnapshot::new(3, 2, (20.0, 0.0, 0.0)));

        world.enter(2);
        world.enter(3);
        world.take();

        // Ascending pool, popped from the back: 254 then 253; 0xff never.
        assert_eq!(world.witness.cache_id_alias(2), Some(254));
        assert_eq!(world.witness.cache_id_alias(3), Some(253));

        // A non-volatile entity gets no alias at all.
        let mut ground = EntitySnapshot::new(4, 2, (30.0, 0.0, 0.0));
        ground.volatile = VolatileInfo::NONE;
        world.witness.upsert_entity(ground);
        world.enter(4);
        assert_eq!(
            world.take()[..1],
            [ClientMessage::EnterAoI {
                id: 4,
                id_alias: NO_ID_ALIAS
            }][..]
        );

        // Freed aliases return to the top of the pool: 254 freed, then 253,
        // so the next enter hands out 253 again.
        world.leave(2);
        world.leave(3);
        world.take();
        world.witness.add_to_aoi(2, false);
        world.witness.flush_state_changes();
        assert_eq!(world.witness.cache_id_alias(2), Some(253));
    }

    #[test]
    fn add_and_remove_mismatches_are_recorded_not_panicked() {
        let mut world = WitnessWorld::new();

        // An entity we have never heard of is dropped silently, both ways.
        world.witness.add_to_aoi(99, false);
        world.witness.remove_from_aoi(99, false);
        assert!(world.witness.warnings().is_empty());

        // A visible entity that was never added is a critical.
        world.witness.remove_from_aoi(2, false);
        assert_eq!(world.witness.warnings().len(), 1);
        assert!(world.witness.warnings()[0].contains("not in AoI"));
    }

    #[test]
    fn request_entity_update_ignores_unknown_and_unpending_requests() {
        let mut world = WitnessWorld::new();

        // Unknown entity.
        world.witness.request_entity_update(2, &[]);
        assert!(world.witness.warnings()[0].contains("no longer in AoI"));

        // The witness's own entity is singled out as a bad client.
        world.witness.request_entity_update(1, &[]);
        assert!(world.witness.warnings()[1].contains("itself"));

        // An updatable cache (already created) is not pending either.
        world.enter(2);
        world.take();
        world.witness.request_entity_update(2, &[]);
        assert!(world.witness.warnings()[2].contains("not pending"));
    }

    #[test]
    fn request_entity_update_clamps_a_lod_stamp_cheat() {
        let mut world = WitnessWorld::new();

        let mut entity = world.witness.entity(2).cloned().unwrap();
        entity.lod_levels = 2;
        world.witness.upsert_entity(entity);

        world.witness.add_to_aoi(2, false);
        world.witness.flush_state_changes();
        world.take();

        // Two levels max, three stamps sent: the excess is dropped with a
        // warning and the stamps land at the top levels.
        world.witness.request_entity_update(2, &[7, 8, 9]);
        assert!(
            world
                .witness
                .warnings()
                .last()
                .unwrap()
                .contains("LoD stamp cheat")
        );

        world.witness.flush_state_changes();
        assert!(matches!(
            world.witness.bundle().first(),
            Some(ClientMessage::CreateEntity { .. })
        ));
        world.take();
    }

    #[test]
    fn a_destroyed_entity_add_is_deferred_through_the_offload_flag() {
        let mut world = WitnessWorld::new();
        let mut dead = world.witness.entity(2).cloned().unwrap();
        dead.destroyed = true;
        world.witness.upsert_entity(dead);

        world.witness.add_to_aoi(2, false);
        assert!(world.witness.is_in_aoi_offload);
        assert!(world.witness.aoi_ids().is_empty());

        // The matched remove clears the flag without touching the AoI.
        world.witness.remove_from_aoi(2, false);
        assert!(!world.witness.is_in_aoi_offload);
        assert!(world.witness.warnings().is_empty());
    }

    #[test]
    fn entities_that_cannot_be_on_the_client_are_ignored() {
        let mut world = WitnessWorld::new();
        let mut hidden = world.witness.entity(2).cloned().unwrap();
        hidden.can_be_on_client = false;
        world.witness.upsert_entity(hidden);

        world.witness.add_to_aoi(2, false);
        assert!(world.witness.aoi_ids().is_empty());
        world.witness.remove_from_aoi(2, false);
        assert!(world.witness.warnings().is_empty());
        world.witness.flush_state_changes();
        assert!(world.witness.bundle().is_empty());
    }

    #[test]
    fn a_newly_seen_entity_joins_at_the_minimum_current_priority() {
        let mut world = WitnessWorld::new();
        world.witness.add_to_aoi(2, false);
        world.witness.cache_mut(2).set_priority(7.5);

        // Entity 3 joins a queue that already holds 2 at priority 7.5: it
        // takes that minimum instead of starting from zero.
        world.witness.aoi_map.insert(
            3,
            EntityCache::new(&EntitySnapshot::new(3, 2, (0.0, 0.0, 0.0))),
        );
        world.witness.add_to_seen(3);
        assert_eq!(world.witness.cache_priority(3), Some(7.5));
        assert_eq!(world.witness.queue_ids(), vec![2, 3]);

        // On an empty queue the newcomer starts at zero.
        let mut fresh = WitnessWorld::new();
        fresh.witness.aoi_map.insert(
            3,
            EntityCache::new(&EntitySnapshot::new(3, 2, (0.0, 0.0, 0.0))),
        );
        fresh.witness.add_to_seen(3);
        assert_eq!(fresh.witness.cache_priority(3), Some(0.0));
    }

    #[test]
    fn creating_a_cache_accumulates_the_default_scheme_delta() {
        let mut world = WitnessWorld::new();
        world.witness.add_to_aoi(2, false);
        world.witness.flush_state_changes();
        world.witness.request_entity_update(2, &[]);

        // Witness at the origin, entity at x=10: dist 10 -> (10/5 + 1) * 1.
        world.witness.flush_state_changes();
        let priority = world.witness.cache_priority(2).unwrap();
        assert!((priority - 3.0).abs() < 1e-9, "{priority}");
        world.take();
    }

    #[test]
    fn leave_stamps_mix_lod_numbers_and_the_last_event() {
        let mut world = WitnessWorld::new();

        let mut entity = world.witness.entity(2).cloned().unwrap();
        entity.lod_levels = 2;
        entity.last_event_number = 42;
        world.witness.upsert_entity(entity);

        world.witness.add_to_aoi(2, false);
        world.witness.flush_state_changes();

        // Both levels stamped by the client; after create the cache sits at
        // detail level 1, so leaving reports the stored stamp for level 0 and
        // the entity's current event for level 1.
        world.witness.request_entity_update(2, &[5, 11]);
        world.witness.flush_state_changes();
        world.take();

        world.leave(2);
        assert_eq!(
            world.take(),
            vec![ClientMessage::LeaveAoI {
                id: 2,
                lod_event_numbers: vec![5, 42],
            }]
        );
    }

    #[test]
    fn a_vehicle_change_before_create_is_announced_first() {
        let mut world = WitnessWorld::new();

        // The entity boards a vehicle between enter and create.
        world.witness.add_to_aoi(2, false);
        world.witness.flush_state_changes();
        world.take();
        let mut entity = world.witness.entity(2).cloned().unwrap();
        entity.vehicle_id = Some(5);
        entity.vehicle_change_num = 1;
        world.witness.upsert_entity(entity);

        world.witness.request_entity_update(2, &[]);
        world.witness.flush_state_changes();

        assert_eq!(
            world.take(),
            vec![
                ClientMessage::SetVehicle {
                    passenger_id: 2,
                    vehicle_id: Some(5),
                },
                ClientMessage::CreateEntity {
                    id: 2,
                    client_type_id: 2,
                    position: (10.0, 0.0, 0.0),
                    yaw: 0.0,
                    pitch: 0.0,
                    roll: 0.0,
                },
            ]
        );
    }

    #[test]
    fn an_enter_on_a_vehicle_uses_the_vehicle_message() {
        let mut world = WitnessWorld::new();
        let mut rider = world.witness.entity(2).cloned().unwrap();
        rider.vehicle_id = Some(5);
        world.witness.upsert_entity(rider);

        world.witness.add_to_aoi(2, false);
        world.witness.flush_state_changes();
        assert_eq!(
            world.take(),
            vec![ClientMessage::EnterAoIOnVehicle {
                id: 2,
                vehicle_id: 5,
                id_alias: 254,
            }]
        );
    }

    #[test]
    fn the_default_scheme_matches_the_reference_curves() {
        let scheme = default_aoi_scheme();
        // weighting 1, distanceWeighting (101/1 - 1)/500 = 0.2.
        assert!((scheme.apply(0.0) - 1.0).abs() < 1e-9);
        assert!((scheme.apply(100.0) - 3.0).abs() < 1e-9); // dist 10
        assert!((scheme.apply(250_000.0) - 101.0).abs() < 1e-9); // dist 500

        // A degenerate scheme is coincident: flat delta 1.
        let flat = AoIUpdateScheme::init(0.0, 0.0, DEFAULT_AOI_RADIUS);
        assert!(flat.should_treat_as_coincident());
        assert_eq!(flat.apply(1000.0), 1.0);
    }

    #[test]
    fn resetting_client_state_resnaps_to_the_entity() {
        let mut entity = EntitySnapshot::new(2, 2, (0.0, 0.0, 0.0));
        entity.last_event_number = 9;
        entity.volatile_update_number = 4;
        entity.lod_levels = 2;
        entity.aoi_update_scheme_id = 3;

        let mut cache = EntityCache::new(&entity);
        assert_eq!(cache.last_event_number(), 9);
        assert_eq!(cache.last_volatile_update_number(), 3);
        assert_eq!(cache.update_scheme_id(), 3);
        assert_eq!(cache.detail_level(), 2);
        assert_eq!(cache.id_alias(), NO_ID_ALIAS);
        assert!(cache.is_updatable());
        assert_eq!(cache.priority(), 0.0);

        // Drift the cache, then remove it from the client: WITHHELD survives
        // (a server-side flag) while the pending state and stamps resnap.
        cache.set_flag(WITHHELD | CREATE_PENDING);
        cache.set_lod_event_numbers(&entity, &[3, 4]);
        cache.set_id_alias(7);
        cache.on_entity_removed_from_client(&entity);
        assert_eq!(cache.flags(), WITHHELD);
        assert_eq!(cache.id_alias(), NO_ID_ALIAS);
        assert_eq!(cache.detail_level(), 2);
        assert_eq!(cache.last_event_number(), 9);
    }

    #[test]
    fn a_very_distant_entity_clamps_its_priority_distance() {
        let mut world = WitnessWorld::new();
        let mut far = EntitySnapshot::new(2, 2, (1e20, 0.0, 0.0));
        far.volatile = VolatileInfo::NONE;
        world.witness.upsert_entity(far);

        world.witness.add_to_aoi(2, false);
        world.witness.flush_state_changes();
        world.witness.request_entity_update(2, &[]);

        // (1e20)^2 overflows to inf; getDistanceSquared clamps to FLT_MAX so
        // the scheme delta stays finite instead of poisoning the priority.
        world.witness.flush_state_changes();
        let priority = world.witness.cache_priority(2).unwrap();
        assert!(priority.is_finite(), "{priority}");
        world.take();
    }

    #[test]
    fn set_aoi_radius_keeps_the_bookkeeping() {
        let mut world = WitnessWorld::new();
        assert_eq!(world.witness.aoi_radius(), 500.0);
        assert_eq!(world.witness.aoi_hyst(), 5.0);

        world.witness.set_aoi_radius(60.0, 8.0);
        assert_eq!(world.witness.aoi_radius(), 60.0);
        assert_eq!(world.witness.aoi_hyst(), 8.0);

        // The reference clamps to at least 0.1.
        world.witness.set_aoi_radius(0.0, 8.0);
        assert!((world.witness.aoi_radius() - 0.1).abs() < 1e-6);
    }

    #[test]
    fn the_lod_priority_is_the_plain_distance_squared() {
        let mut world = WitnessWorld::new();
        world.witness.add_to_aoi(2, false);
        world.witness.flush_state_changes();
        world.take();

        let entity = world.witness.entity(2).cloned().unwrap();
        let cache = world.witness.aoi_map.get(&2).expect("cache after add");
        assert_eq!(cache.lod_priority(&entity, (0.0, 0.0, 0.0)), 100.0);
        assert_eq!(world.witness.entity_id(), 1);
    }

    /// Three observers at 10/50/100m: scheme deltas 3, 11, 21. One 25-byte
    /// update fits a 30-byte packet, so each tick sends exactly one. The
    /// caches are seeded already-updatable (the enter handshake already ran)
    /// so the pass goes straight to the prioritised updates.
    fn prioritised_world() -> WitnessWorld {
        let mut world = WitnessWorld::new();
        world
            .witness
            .upsert_entity(EntitySnapshot::new(3, 2, (50.0, 0.0, 0.0)));
        world
            .witness
            .upsert_entity(EntitySnapshot::new(4, 2, (100.0, 0.0, 0.0)));
        for id in [2, 3, 4] {
            let entity = world.witness.entity(id).cloned().unwrap();
            world.witness.aoi_map.insert(id, EntityCache::new(&entity));
            world.witness.queue_push(id);
        }
        // 4400 bps at 10 Hz -> 4400/80 - 28 = 27 payload bytes per tick: a
        // 25-byte update goes out, then the gate shuts for the tick.
        world.witness.set_witness_capacity(4400, 10);
        world.witness.set_entity_update_bytes(25);
        world
    }

    fn update_ids(world: &mut WitnessWorld) -> Vec<EntityId> {
        world.witness.update();
        world
            .witness
            .take_bundle()
            .into_iter()
            .filter_map(|m| match m {
                ClientMessage::EntityUpdate { id, .. } => Some(id),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn updates_go_out_in_priority_order_within_the_delta_window() {
        let mut world = prioritised_world();

        // Everyone starts at zero, so the first three ticks hand out the
        // entities round-robin in id order.
        let mut sequence = Vec::new();
        for _ in 0..3 {
            sequence.extend(update_ids(&mut world));
        }
        assert_eq!(sequence, vec![2, 3, 4]);

        // Priorities now read 3/11/21. The window only reaches three past
        // the closest, so entity 2 keeps the channel until its accumulated
        // debt crosses entity 3's, and so on: closest-first round robin.
        // Traced by hand against the reference rules: within the window the
        // closest entity (lowest id on ties) keeps the channel until its
        // accumulated debt crosses a neighbour's, and a sent entity clamps
        // up to the earliest queued priority so it never repeats "in past".
        let mut rest = Vec::new();
        for _ in 0..10 {
            rest.extend(update_ids(&mut world));
        }
        assert_eq!(rest, vec![2, 2, 3, 2, 2, 4, 3, 2, 2, 3]);
    }

    #[test]
    fn an_emptied_queue_resets_priorities_to_zero() {
        let mut world = prioritised_world();
        // A fat packet lets the whole queue drain in one tick.
        world.witness.set_witness_capacity(1_000_000, 10);

        for id in [2, 3, 4] {
            world.witness.cache_mut(id).set_priority(40.0 + id as f64);
        }
        update_ids(&mut world);
        for id in [2, 3, 4] {
            assert_eq!(world.witness.cache_priority(id), Some(0.0));
        }
    }

    #[test]
    fn a_sent_entity_is_clamped_to_the_earliest_queued_priority() {
        let mut world = prioritised_world();

        update_ids(&mut world);
        assert_eq!(world.witness.cache_priority(2), Some(3.0));

        // The queue never emptied, so entity 2 (sent this tick at 3.0) may
        // not sit "in the past" next to the two unsent zeros: it clamps up.
        assert_eq!(world.witness.cache_priority(3), Some(0.0));
        assert_eq!(world.witness.cache_priority(4), Some(0.0));
    }

    #[test]
    fn prioritised_vehicles_go_first_and_keep_their_debt() {
        let mut world = prioritised_world();
        world.witness.set_witness_capacity(1_000_000, 10);
        world.witness.set_entity_update_bytes(25);

        // Entity 5 rides as our vehicle with a big priority debt; entity 2
        // is fresh. Give 5 a cache and a queue slot without an enter.
        let rider = EntitySnapshot::new(5, 2, (5.0, 0.0, 0.0));
        world.witness.upsert_entity(rider.clone());
        world.witness.aoi_map.insert(5, EntityCache::new(&rider));
        world.witness.cache_mut(5).set_priority(50.0);
        world.witness.queue_push(5);

        let mut self_entity = world.witness.entity(1).cloned().unwrap();
        self_entity.vehicle_id = Some(5);
        world.witness.upsert_entity(self_entity);

        // The vehicle goes out first regardless of its 50.0 debt; the rest
        // of the (fat-packet) queue follows in id order at zero.
        let ids = update_ids(&mut world);
        assert_eq!(ids, vec![5, 2, 3, 4]);

        // The vehicle was prioritised once; its marker cleared and its debt
        // untouched (it never entered the delta window).
        assert!(!world.witness.aoi_map.get(&5).unwrap().has_flag(PRIORITISED));
        assert_eq!(world.witness.cache_priority(5), Some(50.0));
    }

    #[test]
    fn a_prioritised_vehicle_inside_the_window_restarts_from_the_front() {
        let mut world = prioritised_world();
        world.witness.set_witness_capacity(1_000_000, 10);
        world.witness.set_entity_update_bytes(25);

        // Same vehicle as the debt test, but with no priority debt: it sits
        // inside the delta window, so the pop loop reaches it too.
        let rider = EntitySnapshot::new(5, 2, (5.0, 0.0, 0.0));
        world.witness.upsert_entity(rider.clone());
        world.witness.aoi_map.insert(5, EntityCache::new(&rider));
        world.witness.queue_push(5);

        let mut self_entity = world.witness.entity(1).cloned().unwrap();
        self_entity.vehicle_id = Some(5);
        world.witness.upsert_entity(self_entity);

        // Sent once from the stack; the later pop does not resend, it
        // restarts from the front's priority and drops the marker.
        assert_eq!(update_ids(&mut world), vec![5, 2, 3, 4]);
        let cache = world.witness.aoi_map.get(&5).unwrap();
        assert!(!cache.has_flag(PRIORITISED));
        // the queue emptied this tick, so the emptied-queue reset zeroes
        // every popped entry, the vehicle included
        assert_eq!(world.witness.cache_priority(5), Some(0.0));
    }

    #[test]
    fn the_budget_gates_and_carries_a_deficit() {
        let mut world = WitnessWorld::new();
        // 4240 bps at 10 Hz -> 4240/80 - 28 = 25 payload bytes per tick.
        world.witness.set_witness_capacity(4240, 10);
        world.witness.set_entity_update_bytes(10);
        world
            .witness
            .upsert_entity(EntitySnapshot::new(3, 2, (0.0, 0.0, 0.0)));
        world
            .witness
            .upsert_entity(EntitySnapshot::new(4, 2, (0.0, 0.0, 0.0)));
        for id in [2, 3, 4] {
            world.witness.aoi_map.insert(
                id,
                EntityCache::new(&EntitySnapshot::new(id, 2, (0.0, 0.0, 0.0))),
            );
            world.witness.queue_push(id);
        }

        // Gate closes at 23 bytes: two updates fit, the third would be 30.
        assert_eq!(update_ids(&mut world).len(), 3);
        assert_eq!(world.witness.bandwidth_deficit(), 5);
        assert_eq!(world.witness.max_packet_size(), 25);

        // The deficit shrinks next tick's budget to 20, closing the gate
        // after two updates instead of three.
        assert_eq!(update_ids(&mut world).len(), 2);
        assert_eq!(world.witness.bandwidth_deficit(), 0);
    }

    #[test]
    fn an_oversized_deficit_caps_at_the_packet_size() {
        let mut world = WitnessWorld::new();
        world.witness.set_witness_capacity(4240, 10);
        world.witness.set_entity_update_bytes(200);
        world.witness.aoi_map.insert(
            2,
            EntityCache::new(&EntitySnapshot::new(2, 2, (0.0, 0.0, 0.0))),
        );
        world.witness.queue_push(2);

        // One 200-byte update against a 25-byte packet: the deficit caps at
        // one full packet with a warning.
        update_ids(&mut world);
        assert_eq!(world.witness.bandwidth_deficit(), 25);
        assert!(world.witness.warnings().last().unwrap().contains("deficit"));
    }

    #[test]
    fn the_emergency_throttle_shrinks_the_budget() {
        let mut world = WitnessWorld::new();
        world.witness.set_witness_capacity(4240, 10);
        world.witness.set_entity_update_bytes(10);
        for id in [2, 3, 4] {
            world.witness.aoi_map.insert(
                id,
                EntityCache::new(&EntitySnapshot::new(id, 2, (0.0, 0.0, 0.0))),
            );
            world.witness.queue_push(id);
        }

        world.witness.set_emergency_throttle(0.5);
        // desired = 12, gate closes at 10: one update only.
        assert_eq!(update_ids(&mut world).len(), 1);
    }

    #[test]
    fn state_handshakes_ride_the_update_pass() {
        let mut world = WitnessWorld::new();
        world.witness.set_witness_capacity(1_000_000, 10);

        world.witness.add_to_aoi(2, false);
        // the enterAoI handshake rides the pass; no entity update yet
        assert!(update_ids(&mut world).is_empty());
        assert!(
            !world
                .witness
                .aoi_map
                .get(&2)
                .unwrap()
                .has_flag(ENTER_PENDING)
        );
        world.take();

        world.witness.request_entity_update(2, &[]);
        world.witness.update();
        assert!(matches!(
            world.witness.bundle().first(),
            Some(ClientMessage::CreateEntity { .. })
        ));
        assert!(world.witness.bundle_size() > 0);
        world.take();
    }

    #[test]
    fn a_destroyed_manual_entity_is_dropped_inside_the_pass() {
        let mut world = WitnessWorld::new();
        world.witness.set_witness_capacity(1_000_000, 10);

        world.witness.add_to_aoi(2, true);
        let mut dead = world.witness.entity(2).cloned().unwrap();
        dead.destroyed = true;
        world.witness.upsert_entity(dead);

        world.witness.update();
        assert!(world.witness.bundle().is_empty());
        assert!(!world.witness.aoi_ids().contains(&2));
    }

    #[test]
    fn an_unupdatable_vehicle_is_skipped_in_the_stack() {
        let mut world = prioritised_world();

        let rider = EntitySnapshot::new(5, 2, (5.0, 0.0, 0.0));
        world.witness.upsert_entity(rider.clone());
        world.witness.aoi_map.insert(5, EntityCache::new(&rider));
        world.witness.cache_mut(5).set_flag(WITHHELD);
        world.witness.queue_push(5);

        let mut self_entity = world.witness.entity(1).cloned().unwrap();
        self_entity.vehicle_id = Some(5);
        world.witness.upsert_entity(self_entity);

        // The withheld vehicle is not sent from the stack; the budget
        // still only fits the closest entity this tick.
        let ids = update_ids(&mut world);
        assert_eq!(ids, vec![2]);
        // A witness on a vehicle has to send its reference position first,
        // and that 8 bytes pushes the update over the 27-byte packet.
        assert_eq!(world.witness.bandwidth_deficit(), 6);
    }

    // ---- BW-08-03-04: position and property projections ----

    /// Two seeded volatile entities, `witness.entities` style.
    fn projection_world(self_position: (f32, f32, f32)) -> WitnessWorld {
        let mut self_entity = EntitySnapshot::new(1, 1, self_position);
        self_entity.volatile = VolatileInfo::NONE;
        let mut witness = Witness::new(1, self_entity);
        witness.set_witness_capacity(1_000_000, 10);
        WitnessWorld { witness }
    }

    fn seed_observer(world: &mut WitnessWorld, entity: EntitySnapshot) {
        world.witness.upsert_entity(entity.clone());
        world
            .witness
            .aoi_map
            .insert(entity.id, EntityCache::new(&entity));
        world.witness.queue_push(entity.id);
    }

    fn first_entity_update(world: &mut WitnessWorld) -> ClientMessage {
        world.witness.update();
        world
            .witness
            .take_bundle()
            .into_iter()
            .find(|m| matches!(m, ClientMessage::EntityUpdate { .. }))
            .expect("one entity update went out")
    }

    #[test]
    fn two_witnesses_each_get_their_own_projection() {
        let entity = EntitySnapshot::new(2, 1, (20.0, 0.0, 0.0));

        // Witness A sits at the grid origin; the offset is 20.
        let mut a = projection_world((0.0, 0.0, 0.0));
        seed_observer(&mut a, entity.clone());
        match first_entity_update(&mut a) {
            ClientMessage::EntityUpdate { id, pos, dir, .. } => {
                assert_eq!(id, 2);
                assert_eq!(
                    pos,
                    PosProjection::Relative {
                        x: 20.0,
                        y: 0.0,
                        z: 0.0,
                        on_ground: true
                    }
                );
                assert_eq!(
                    dir,
                    DirProjection::YawPitchRoll {
                        yaw: 0.0,
                        pitch: 0.0,
                        roll: 0.0
                    }
                );
            }
            other => panic!("unexpected message: {other:?}"),
        }

        // Witness B sits at (15, 0, 0); the same entity projects to 5.
        let mut b = projection_world((15.0, 0.0, 0.0));
        seed_observer(&mut b, entity);
        match first_entity_update(&mut b) {
            ClientMessage::EntityUpdate { pos, .. } => assert_eq!(
                pos,
                PosProjection::Relative {
                    x: 5.0,
                    y: 0.0,
                    z: 0.0,
                    on_ground: true
                }
            ),
            other => panic!("unexpected message: {other:?}"),
        }
    }

    #[test]
    fn an_offset_beyond_the_packed_limits_falls_back_to_detailed() {
        let mut world = projection_world((0.0, 0.0, 0.0));
        // packedXZScale keeps the packed form inside the 500m AoI, so 600m
        // cannot be represented.
        seed_observer(&mut world, EntitySnapshot::new(2, 1, (600.0, 0.0, 0.0)));
        match first_entity_update(&mut world) {
            ClientMessage::EntityUpdate { pos, .. } => {
                assert_eq!(
                    pos,
                    PosProjection::Detailed {
                        x: 600.0,
                        y: 0.0,
                        z: 0.0
                    }
                );
            }
            other => panic!("unexpected message: {other:?}"),
        }
    }

    #[test]
    fn direction_detail_shrinks_with_distance() {
        // position ALWAYS; direction thresholds in metres
        let thresholds =
            VolatileInfo::from_thresholds(Some(-1.0), Some(10.0), Some(8.0), Some(5.0));

        // 25 < every squared threshold: all three angles go out.
        let mut near = EntitySnapshot::new(2, 1, (3.0, 0.0, 0.0));
        near.volatile = thresholds;
        let mut world = projection_world((0.0, 0.0, 0.0));
        seed_observer(&mut world, near);
        match first_entity_update(&mut world) {
            ClientMessage::EntityUpdate { dir, .. } => assert_eq!(
                dir,
                DirProjection::YawPitchRoll {
                    yaw: 0.0,
                    pitch: 0.0,
                    roll: 0.0
                }
            ),
            other => panic!("unexpected message: {other:?}"),
        }

        // 81 > 64 and > 25 but < 100: yaw only.
        let mut mid = EntitySnapshot::new(3, 1, (9.0, 0.0, 0.0));
        mid.volatile = thresholds;
        let mut world = projection_world((0.0, 0.0, 0.0));
        seed_observer(&mut world, mid);
        match first_entity_update(&mut world) {
            ClientMessage::EntityUpdate { dir, .. } => {
                assert_eq!(dir, DirProjection::Yaw { yaw: 0.0 });
            }
            other => panic!("unexpected message: {other:?}"),
        }

        // 32 clears only the roll (25): yaw and pitch.
        let mut mid_low = EntitySnapshot::new(5, 1, (4.0, 0.0, 4.0));
        mid_low.volatile = thresholds;
        let mut world = projection_world((0.0, 0.0, 0.0));
        seed_observer(&mut world, mid_low);
        match first_entity_update(&mut world) {
            ClientMessage::EntityUpdate { dir, .. } => {
                assert_eq!(
                    dir,
                    DirProjection::YawPitch {
                        yaw: 0.0,
                        pitch: 0.0
                    }
                );
            }
            other => panic!("unexpected message: {other:?}"),
        }

        // 720000 clears everything: no direction at all.
        let mut far = EntitySnapshot::new(6, 1, (600.0, 0.0, 600.0));
        far.volatile = thresholds;
        let mut world = projection_world((0.0, 0.0, 0.0));
        seed_observer(&mut world, far);
        match first_entity_update(&mut world) {
            ClientMessage::EntityUpdate { dir, pos, .. } => {
                assert_eq!(dir, DirProjection::None);
                // both offsets are past the 500m packed limit
                assert!(matches!(pos, PosProjection::Detailed { .. }));
            }
            other => panic!("unexpected message: {other:?}"),
        }

        // A type that sends no position at all: pos drops, dir stays.
        let mut ground = EntitySnapshot::new(4, 1, (3.0, 0.0, 0.0));
        ground.volatile = VolatileInfo::from_thresholds(None, Some(5.0), Some(4.0), Some(3.0));
        let mut world = projection_world((0.0, 0.0, 0.0));
        seed_observer(&mut world, ground);
        match first_entity_update(&mut world) {
            ClientMessage::EntityUpdate { pos, dir, .. } => {
                assert_eq!(pos, PosProjection::None);
                assert!(matches!(dir, DirProjection::YawPitchRoll { .. }));
            }
            other => panic!("unexpected message: {other:?}"),
        }
    }

    #[test]
    fn an_airborne_entity_packs_three_axes() {
        let mut world = projection_world((0.0, 0.0, 0.0));
        let mut flyer = EntitySnapshot::new(2, 1, (6.0, 12.0, 8.0));
        flyer.is_on_ground = false;
        seed_observer(&mut world, flyer);
        match first_entity_update(&mut world) {
            ClientMessage::EntityUpdate { pos, bytes, .. } => {
                assert_eq!(
                    pos,
                    PosProjection::Relative {
                        x: 6.0,
                        y: 12.0,
                        z: 8.0,
                        on_ground: false
                    }
                );
                // alias 4 + PackedXYZ 5 + YawPitchRoll 12
                assert_eq!(bytes, 21);
            }
            other => panic!("unexpected message: {other:?}"),
        }
    }

    #[test]
    fn a_vehicle_passenger_projects_its_absolute_position() {
        let mut world = projection_world((0.0, 0.0, 0.0));
        let mut rider = EntitySnapshot::new(2, 1, (20.0, 0.0, 0.0));
        rider.vehicle_id = Some(3);
        seed_observer(&mut world, rider.clone());
        // The cache already knows about the vehicle, so no SetVehicle rides.
        world.witness.cache_mut(2).vehicle_change_num = rider.vehicle_change_num;
        match first_entity_update(&mut world) {
            ClientMessage::EntityUpdate { pos, .. } => {
                assert_eq!(
                    pos,
                    PosProjection::Absolute {
                        x: 20.0,
                        y: 0.0,
                        z: 0.0,
                        on_ground: true
                    }
                );
            }
            other => panic!("unexpected message: {other:?}"),
        }
    }

    #[test]
    fn an_always_detailed_cache_projects_an_absolute_position() {
        let mut world = projection_world((0.0, 0.0, 0.0));
        let entity = EntitySnapshot::new(2, 1, (20.0, 0.0, 0.0));
        seed_observer(&mut world, entity.clone());
        world.witness.cache_mut(2).set_flag(IS_ALWAYS_DETAILED);
        match first_entity_update(&mut world) {
            ClientMessage::EntityUpdate { pos, .. } => {
                assert_eq!(
                    pos,
                    PosProjection::Detailed {
                        x: 20.0,
                        y: 0.0,
                        z: 0.0
                    }
                );
            }
            other => panic!("unexpected message: {other:?}"),
        }

        // A prioritised vehicle is only detailed when the config asks for
        // it; the default does not.
        let mut world = projection_world((0.0, 0.0, 0.0));
        let mut vehicle = EntitySnapshot::new(3, 1, (10.0, 0.0, 0.0));
        vehicle.vehicle_id = Some(4);
        seed_observer(&mut world, vehicle.clone());
        let mut self_entity = world.witness.entity(1).cloned().unwrap();
        self_entity.vehicle_id = Some(3);
        world.witness.upsert_entity(self_entity);
        world.witness.cache_mut(3).vehicle_change_num = vehicle.vehicle_change_num;
        match first_entity_update(&mut world) {
            ClientMessage::EntityUpdate { pos, .. } => {
                assert_eq!(
                    pos,
                    PosProjection::Absolute {
                        x: 10.0,
                        y: 0.0,
                        z: 0.0,
                        on_ground: true
                    }
                );
            }
            other => panic!("unexpected message: {other:?}"),
        }
    }

    #[test]
    fn the_self_projection_rides_ahead_of_the_updates() {
        let mut self_entity = EntitySnapshot::new(1, 1, (3.0, 4.0, 5.0));
        self_entity.yaw = 1.0;
        self_entity.pitch = 0.5;
        self_entity.roll = 0.25;
        let mut world = WitnessWorld {
            witness: Witness::new(1, self_entity.clone()),
        };
        world.witness.set_witness_capacity(1_000_000, 10);
        world
            .witness
            .upsert_entity(EntitySnapshot::new(2, 1, (10.0, 0.0, 0.0)));
        let entity = world.witness.entity(2).cloned().unwrap();
        world.witness.aoi_map.insert(2, EntityCache::new(&entity));
        world.witness.queue_push(2);

        world.witness.update();
        let bundle = world.witness.take_bundle();
        assert_eq!(
            bundle.first(),
            Some(&ClientMessage::AvatarUpdatePlayerDetailed {
                position: (3.0, 4.0, 5.0),
                yaw: 1.0,
                pitch: 0.5,
                roll: 0.25
            })
        );
        assert!(matches!(
            bundle.get(1),
            Some(ClientMessage::EntityUpdate { id: 2, .. })
        ));
    }

    #[test]
    fn an_unchanged_player_position_is_skipped_when_recent() {
        let mut self_entity = EntitySnapshot::new(1, 1, (3.0, 0.0, 0.0));
        self_entity.vehicle_id = Some(2); // a reference position rides anyway
        let mut world = WitnessWorld {
            witness: Witness::new(1, self_entity),
        };
        world.witness.set_max_time_between_player_updates(10);

        world.witness.update();
        let bundle = world.witness.take_bundle();
        assert!(matches!(
            bundle.first(),
            Some(ClientMessage::RelativePosition { .. })
        ));
        assert!(matches!(
            bundle.get(1),
            Some(ClientMessage::AvatarUpdatePlayerDetailed { .. })
        ));

        // The same tick's worth of budget: recent and unchanged, so the
        // player position is not repeated.
        world.witness.update();
        let bundle = world.witness.take_bundle();
        assert!(
            !bundle
                .iter()
                .any(|m| matches!(m, ClientMessage::AvatarUpdatePlayerDetailed { .. }))
        );

        // Moving reopens the tap.
        let mut self_entity = world.witness.entity(1).cloned().unwrap();
        self_entity.position = (4.0, 0.0, 0.0);
        world.witness.upsert_entity(self_entity);
        world.witness.update();
        let bundle = world.witness.take_bundle();
        assert!(matches!(
            bundle.get(1),
            Some(ClientMessage::AvatarUpdatePlayerDetailed {
                position: (4.0, 0.0, 0.0),
                ..
            })
        ));
    }

    #[test]
    fn a_client_controlled_witness_uses_the_client_reference() {
        let mut self_entity = EntitySnapshot::new(1, 1, (0.0, 0.0, 0.0));
        self_entity.controlled_by_self = true;
        let mut world = WitnessWorld {
            witness: Witness::new(1, self_entity),
        };

        // Before the client has ever sent a reference we pick one ourselves.
        world.witness.update();
        let bundle = world.witness.take_bundle();
        assert_eq!(
            bundle.first(),
            Some(&ClientMessage::RelativePosition {
                position: (0.0, 0.0, 0.0)
            })
        );
        // And a client-controlled entity never gets its position repeated.
        assert!(
            !bundle
                .iter()
                .any(|m| matches!(m, ClientMessage::AvatarUpdatePlayerDetailed { .. }))
        );

        // Once the client supplies its sequence, only that rides.
        world.witness.set_client_reference_sequence(7);
        world.witness.update();
        let bundle = world.witness.take_bundle();
        assert_eq!(
            bundle.first(),
            Some(&ClientMessage::RelativePositionReference { sequence_number: 7 })
        );
    }

    #[test]
    fn history_events_ride_the_projection_in_order() {
        let mut world = projection_world((0.0, 0.0, 0.0));
        let mut entity = EntitySnapshot::new(2, 1, (3.0, 0.0, 0.0));
        entity.volatile = VolatileInfo::NONE; // the select rides first
        entity.events = vec![
            HistoryEvent {
                number: 5,
                level: HistoryLevel::Priority(10_000.0), // squared: 100m
                payload_bytes: 6,
            },
            HistoryEvent {
                number: 7,
                level: HistoryLevel::Detail(1),
                payload_bytes: 2,
            },
        ];
        seed_observer(&mut world, entity.clone());
        // the client knows this entity by alias
        world.witness.cache_mut(2).id_alias = 5;

        world.witness.update();
        let bundle = world.witness.take_bundle();
        assert_eq!(
            bundle.first(),
            Some(&ClientMessage::SelectAliasedEntity { id_alias: 5 })
        );
        match bundle.get(1) {
            Some(ClientMessage::EntityUpdate {
                id,
                id_alias,
                events,
                bytes,
                ..
            }) => {
                assert_eq!(*id, 2);
                assert_eq!(*id_alias, Some(5));
                assert_eq!(events, &[5, 7]);
                // alias 1 + no position + no direction + (4+6) + (4+2)
                assert_eq!(*bytes, 17);
            }
            other => panic!("unexpected message: {other:?}"),
        }

        // The same shape without an alias: the full id rides the select,
        // and the detail increase does not repeat it.
        let mut world = projection_world((0.0, 0.0, 0.0));
        entity.events = vec![entity.events[1]];
        entity.lod_levels = 2;
        entity.lod_thresholds = vec![
            LodLevelThreshold {
                low: -1.0,
                high: 1600.0,
            },
            LodLevelThreshold {
                low: 900.0,
                high: 1600.0,
            },
        ];
        seed_observer(&mut world, entity.clone());
        world.witness.cache_mut(2).detail_level = 1;
        world.witness.upsert_entity(entity);
        world.witness.update();
        let bundle = world.witness.take_bundle();
        assert_eq!(bundle.first(), Some(&ClientMessage::SelectEntity { id: 2 }));
        assert_eq!(
            bundle.get(1),
            Some(&ClientMessage::EntityUpdate {
                id: 2,
                id_alias: None,
                pos: PosProjection::None,
                dir: DirProjection::None,
                events: vec![7],
                bytes: 10,
            })
        );
        assert_eq!(
            bundle.get(2),
            Some(&ClientMessage::UpdateProperties {
                id: 2,
                id_alias: None,
                detail_level: 0
            })
        );
    }

    #[test]
    fn out_of_range_events_are_not_sent_but_the_number_still_advances() {
        let mut world = projection_world((0.0, 0.0, 0.0));
        seed_observer(&mut world, EntitySnapshot::new(2, 1, (3.0, 0.0, 0.0)));

        // squared threshold 16: at distance 3 (9) the event goes out.
        let mut entity = world.witness.entity(2).cloned().unwrap();
        entity.events = vec![HistoryEvent {
            number: 5,
            level: HistoryLevel::Priority(16.0),
            payload_bytes: 4,
        }];
        entity.last_event_number = 5;
        world.witness.upsert_entity(entity);

        let sent = first_entity_update(&mut world);
        match sent {
            ClientMessage::EntityUpdate { events, .. } => assert_eq!(events, &[5]),
            other => panic!("unexpected message: {other:?}"),
        }

        // Step the entity past the threshold with a fresh event; the new
        // one is suppressed, and the cache still catches up.
        let mut entity = world.witness.entity(2).cloned().unwrap();
        entity.position = (5.0, 0.0, 0.0);
        entity.events.push(HistoryEvent {
            number: 6,
            level: HistoryLevel::Priority(16.0),
            payload_bytes: 4,
        });
        entity.last_event_number = 6;
        world.witness.upsert_entity(entity);
        let sent = first_entity_update(&mut world);
        match sent {
            ClientMessage::EntityUpdate { events, .. } => assert!(events.is_empty()),
            other => panic!("unexpected message: {other:?}"),
        }
        let cache = world.witness.aoi_map.get(&2).unwrap();
        assert_eq!(cache.last_event_number(), 6);
    }

    #[test]
    fn the_detail_level_transitions_stamp_and_resend() {
        let mut world = projection_world((0.0, 0.0, 0.0));
        let mut entity = EntitySnapshot::new(2, 1, (5.0, 0.0, 0.0));
        entity.lod_levels = 2;
        entity.lod_thresholds = vec![
            LodLevelThreshold {
                low: -1.0,
                high: 1600.0, // 40m squared
            },
            LodLevelThreshold {
                low: 900.0, // 30m squared
                high: 1600.0,
            },
        ];
        seed_observer(&mut world, entity);
        // `addOuterDetailLevel` leaves entry one level below the outermost.
        world.witness.cache_mut(2).detail_level = 1;
        world.witness.cache_mut(2).id_alias = 7;
        world.witness.update();
        let bundle = world.witness.take_bundle();
        assert_eq!(
            bundle.last(),
            Some(&ClientMessage::UpdateProperties {
                id: 2,
                id_alias: Some(7),
                detail_level: 0
            })
        );
        assert_eq!(world.witness.aoi_map.get(&2).unwrap().detail_level(), 0);

        // Move out past 40m: both levels stamp their events on the way up.
        let mut entity = world.witness.entity(2).cloned().unwrap();
        entity.position = (50.0, 0.0, 0.0);
        world.witness.upsert_entity(entity);
        world.witness.update();
        let cache = world.witness.aoi_map.get(&2).unwrap();
        assert_eq!(cache.detail_level(), 2);
        let stamps = cache.lod_event_numbers();
        assert_eq!(stamps[0], 0);
        assert_eq!(stamps[1], 0);
    }

    #[test]
    fn a_detail_increase_selects_the_entity_first() {
        let mut world = projection_world((0.0, 0.0, 0.0));
        let mut entity = EntitySnapshot::new(2, 1, (5.0, 0.0, 0.0));
        entity.volatile = VolatileInfo::NONE; // no volatile select ahead
        entity.lod_levels = 2;
        entity.lod_thresholds = vec![
            LodLevelThreshold {
                low: -1.0,
                high: 1600.0,
            },
            LodLevelThreshold {
                low: 900.0,
                high: 1600.0,
            },
        ];
        seed_observer(&mut world, entity.clone());
        world.witness.cache_mut(2).detail_level = 1;

        world.witness.update();
        let bundle = world.witness.take_bundle();
        assert!(matches!(
            bundle.first(),
            Some(ClientMessage::EntityUpdate { .. })
        ));
        assert_eq!(bundle.get(1), Some(&ClientMessage::SelectEntity { id: 2 }));
        assert_eq!(
            bundle.get(2),
            Some(&ClientMessage::UpdateProperties {
                id: 2,
                id_alias: None,
                detail_level: 0
            })
        );

        // The aliased shape selects by alias instead.
        let mut world = projection_world((0.0, 0.0, 0.0));
        seed_observer(&mut world, entity.clone());
        world.witness.cache_mut(2).detail_level = 1;
        world.witness.cache_mut(2).id_alias = 7;
        world.witness.upsert_entity(entity);
        world.witness.update();
        let bundle = world.witness.take_bundle();
        assert_eq!(
            bundle.get(1),
            Some(&ClientMessage::SelectAliasedEntity { id_alias: 7 })
        );
        assert_eq!(
            bundle.get(2),
            Some(&ClientMessage::UpdateProperties {
                id: 2,
                id_alias: Some(7),
                detail_level: 0
            })
        );
    }

    #[test]
    fn a_coincident_scheme_projects_from_zero_priority() {
        let mut world = projection_world((0.0, 0.0, 0.0));
        let mut entity = EntitySnapshot::new(2, 1, (600.0, 0.0, 0.0));
        entity.aoi_update_scheme_id = 1; // custom schemes are coincident
        seed_observer(&mut world, entity);

        // Coincident entities read as LoD priority 0, so even a 600m offset
        // keeps every direction angle; the position itself is past the
        // packed limits and goes out detailed.
        match first_entity_update(&mut world) {
            ClientMessage::EntityUpdate { pos, dir, .. } => {
                assert!(matches!(pos, PosProjection::Detailed { .. }));
                assert_eq!(
                    dir,
                    DirProjection::YawPitchRoll {
                        yaw: 0.0,
                        pitch: 0.0,
                        roll: 0.0
                    }
                );
            }
            other => panic!("unexpected message: {other:?}"),
        }
    }

    #[test]
    fn clearing_the_update_size_returns_to_the_computed_payload() {
        let mut world = projection_world((0.0, 0.0, 0.0));
        seed_observer(&mut world, EntitySnapshot::new(2, 1, (10.0, 0.0, 0.0)));
        world.witness.set_witness_capacity(1_000_000, 10);

        // The harness fix charges 25 regardless of the body.
        world.witness.set_entity_update_bytes(25);
        world.witness.update();
        let bundle = world.witness.take_bundle();
        assert!(matches!(
            bundle.first(),
            Some(ClientMessage::EntityUpdate { bytes: 25, .. })
        ));

        world.witness.clear_entity_update_bytes();
        world.witness.update();
        let bundle = world.witness.take_bundle();
        // The volatile number has not moved, so the second body is only the
        // entity id.
        assert!(matches!(
            bundle.first(),
            Some(ClientMessage::EntityUpdate { bytes: 4, .. })
        ));
        // two passes have run
        assert_eq!(world.witness.time(), 3);
    }

    #[test]
    fn the_budget_counts_the_real_payload() {
        let mut world = projection_world((0.0, 0.0, 0.0));
        seed_observer(&mut world, EntitySnapshot::new(2, 1, (10.0, 0.0, 0.0)));
        world.witness.set_witness_capacity(4400, 10); // 27 bytes a tick
        // No harness fix this time: a 19-byte update goes out, and the
        // 8-byte headroom cannot fit a second one.
        let ids = update_ids(&mut world);
        assert_eq!(ids, vec![2]);
        assert_eq!(world.witness.bandwidth_deficit(), 0);
        let cache = world.witness.aoi_map.get(&2).unwrap();
        assert_eq!(cache.priority(), 0.0);
    }

    #[test]
    fn vehicle_changes_ride_updates() {
        let mut world = projection_world((0.0, 0.0, 0.0));
        let mut rider = EntitySnapshot::new(2, 1, (6.0, 0.0, 0.0));
        rider.vehicle_change_num = 3;
        seed_observer(&mut world, rider.clone());

        world.witness.update();
        let bundle = world.witness.take_bundle();
        assert_eq!(
            bundle.first(),
            Some(&ClientMessage::SetVehicle {
                passenger_id: 2,
                vehicle_id: None
            })
        );
        assert!(matches!(
            bundle.get(1),
            Some(ClientMessage::EntityUpdate { .. })
        ));
        // stamped, so the next pass does not repeat it
        world.witness.update();
        let bundle = world.witness.take_bundle();
        assert!(
            !bundle
                .iter()
                .any(|m| matches!(m, ClientMessage::SetVehicle { .. }))
        );
    }

    #[test]
    fn volatile_info_thresholds_match_the_reference() {
        let info = VolatileInfo::from_thresholds(Some(10.0), Some(10.0), Some(8.0), Some(5.0));
        assert_eq!(info.position_priority, 100.0);
        assert!(info.should_send_position());
        // squared distance 100 clears the pitch (64) and roll (25) but not
        // the yaw (100): yaw only.
        assert_eq!(info.dir_type(100.0), 2);
        assert_eq!(info.dir_type(101.0), 3);
        // 20 sits under the roll's 25: every angle rides.
        assert_eq!(info.dir_type(20.0), 0);
        assert!(info.has_volatile(99.0));
        assert!(!info.has_volatile(100.0));

        // -1 means ALWAYS, no section means the axis is off.
        let always = VolatileInfo::from_thresholds(Some(-1.0), Some(-1.0), Some(-1.0), Some(-1.0));
        assert_eq!(always.position_priority, VOLATILE_ALWAYS);
        assert!(always.has_volatile(0.0));
        assert!(!VolatileInfo::NONE.has_volatile(0.0));
        assert!(!VolatileInfo::NONE.should_send_position());
    }

    #[test]
    fn a_vehicle_outside_the_aoi_is_reported_once() {
        let mut world = WitnessWorld::new();
        world.witness.set_witness_capacity(1_000_000, 10);

        let mut self_entity = world.witness.entity(1).cloned().unwrap();
        self_entity.vehicle_id = Some(9);
        world.witness.upsert_entity(self_entity);

        world.witness.update();
        assert!(
            world
                .witness
                .warnings()
                .last()
                .unwrap()
                .contains("outside its AoI")
        );
    }

    #[test]
    fn bps_to_packet_size_matches_the_reference() {
        assert_eq!(bps_to_packet_size(4640, 10), 30);
        assert_eq!(bps_to_packet_size(4240, 10), 25);
        // 8 bits per byte, 28 bytes of UDP/IP overhead.
        assert_eq!(bps_to_packet_size(8000, 10), 100 - 28);

        // a zero hertz leaves the packet size untouched
        let mut world = WitnessWorld::new();
        world.witness.set_witness_capacity(4400, 10);
        world.witness.set_witness_capacity(9999, 0);
        assert_eq!(world.witness.max_packet_size(), 27);
    }

    #[test]
    fn a_custom_scheme_is_treated_as_coincident() {
        let mut world = WitnessWorld::new();
        let mut entity = world.witness.entity(2).cloned().unwrap();
        entity.aoi_update_scheme_id = 1;
        world.witness.upsert_entity(entity);

        world.witness.add_to_aoi(2, false);
        world.witness.flush_state_changes();
        world.witness.request_entity_update(2, &[]);

        // Non-default schemes register through bw.xml, which the port does
        // not parse yet: they sit at a flat delta 1 like a coincident scheme.
        world.witness.flush_state_changes();
        let priority = world.witness.cache_priority(2).unwrap();
        assert!((priority - 1.0).abs() < 1e-9, "{priority}");
        world.take();
    }
}
