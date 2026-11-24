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
    /// `volatileInfo().hasVolatile( 0.f )` — gates alias allocation and the
    /// volatile create message.
    pub has_volatile: bool,
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
            has_volatile: true,
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

/// `AoIUpdateSchemes::apply` — scheme 0 is the default; custom schemes come
/// with bw.xml and are treated as coincident until BW-08-03-03 needs them.
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
}

impl Witness {
    /// `Witness::Witness` for `CREATE_REAL_FROM_INIT`.
    pub fn new(entity_id: EntityId, self_entity: EntitySnapshot) -> Self {
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

    /// `bundle()` — inspect queued messages without consuming them.
    pub fn bundle(&self) -> &[ClientMessage] {
        &self.bundle
    }

    /// Send everything queued and clear the bundle.
    pub fn take_bundle(&mut self) -> Vec<ClientMessage> {
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
        if entity.has_volatile && !self.free_aliases.is_empty() {
            return self.free_aliases.pop().expect("checked non-empty");
        }
        NO_ID_ALIAS
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
            Some(vehicle_id) => self.bundle.push(ClientMessage::EnterAoIOnVehicle {
                id,
                vehicle_id,
                id_alias: alias,
            }),
            None => self.bundle.push(ClientMessage::EnterAoI {
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
            self.bundle.push(ClientMessage::SetVehicle {
                passenger_id: id,
                vehicle_id: entity.vehicle_id,
            });
            self.cache_mut(id)
                .set_vehicle_change_num(entity.vehicle_change_num);
        }

        let (yaw, pitch, roll) = (entity.yaw, entity.pitch, entity.roll);
        let message = if entity.has_volatile {
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
        self.bundle.push(message);

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
            self.bundle.push(ClientMessage::LeaveAoI {
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

    /// `Witness::handleStateChange` — the update pass walks the queue and
    /// dispatches every non-updatable cache through here. Returns true when
    /// the slot was vacated (the reference's swap-pop: the queue's last
    /// element lands in the slot and is handled in the same sweep).
    fn handle_state_change(&mut self, slot: usize, origin: (f32, f32, f32)) -> bool {
        let id = self.entity_queue[slot];
        assert!(!self.cache_mut(id).has_flag(REQUEST_PENDING));

        if self.cache_mut(id).has_flag(GONE) {
            self.delete_from_client(id);
            self.entity_queue.swap_remove(slot);
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
            self.entity_queue.swap_remove(slot);
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
            self.entity_queue.swap_remove(slot);
            true
        } else {
            unreachable!("queued cache with no pending state");
        }
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
            if !cache.is_updatable() && self.handle_state_change(i, origin) {
                // slot vacated: the swapped-in occupant is up next
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
            let mut witness = Witness::new(1, EntitySnapshot::new(1, 1, (0.0, 0.0, 0.0)));
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
        ground.has_volatile = false;
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
        far.has_volatile = false;
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
