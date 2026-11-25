//! The cell-app witness: the Area of Interest one client sees.
//!
//! Reference faces: `server/cellapp/witness.cpp` (`Witness` and its
//! `AoITrigger`), `server/cellapp/entity_cache.{hpp,cpp,ipp}`
//! (`EntityCache`, `EntityCacheMap`), `server/cellapp/
//! aoi_update_schemes.{hpp,cpp}` and `lib/entitydef/data_lod_level.*`
//! (`DataLoDLevels`).
//!
//! The witness owns one `EntityCache` per entity its client currently
//! sees (`aoiMap_`), a min-priority queue of the caches still to send
//! (`entityQueue_`, `PriorityCompare`), and the 256-slot id-alias pool
//! the shorthand client messages address entities by. An entity enters
//! the AoI when the witness's `AoITrigger` — a `RangeTrigger` wanting
//! the lower/upper AoI flags and making none — crosses it, and leaves
//! when the crossing reverses; both ride the [`TriggerTarget::AoI`]
//! queue that [`CellApp::pump_aoi_events`] applies in order.
//!
//! Port divergences, all deliberate:
//! - The reference fires `addToAoI`/`removeFromAoI` reentrantly from
//!   inside the range-list shuffle. Neither handler touches the range
//!   list, so the port records the crossings in a queue and applies
//!   them right after the list op — same events, same order, no
//!   reentrancy.
//! - `AoITrigger`'s constructor `visitLargeEntities` pass (entering the
//!   big entities whose range already spans the observer) has no face
//!   here yet; those entities' crossing reports arrive the same way
//!   once the large-entity lane ports.
//! - The AoI-rooted face (`setAoIRoot`/`clearAoIRoot`/
//!   `replaceAoITrigger` with a `MobileRangeListNode` centre) is
//!   deferred: the trigger is always centred on the witnessing entity's
//!   own range node for now.
//! - `Witness::update`'s writer faces (`addSpaceDataChanges`,
//!   `addReferencePosition`, `addDetailedPlayerPosition`) are
//!   BW-08-03-04's: the pass's bundle starts empty and each send
//!   spends the context's fixed `update_bytes`.
//! - `Entity::writeClientUpdateDataToBundle`'s history-event payload
//!   and the volatile position encoding are BW-08-03-04's face. The LoD
//!   machinery ([`EntityCache::update_detail_level`],
//!   [`EntityCache::add_changed_properties`]) is fully ported against
//!   the explicit [`ClientProperty`] table standing in for
//!   `propertyEventStamps()`; the create path records the outer-level
//!   property indices on [`Wire::CreateEntity`], where the reference
//!   appends them raw inside the create message.
//! - The offload/onload streaming face (`writeAoI`/`readAoI` and the
//!   `EntityCache` streaming operators) is deferred with the offload
//!   lane; `EntityCache::write_to_stream` orders by entity id where the
//!   reference orders by pointer identity.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

use crate::cellapp_range_list::{
    FLAG_LOWER_AOI_TRIGGER, FLAG_NO_TRIGGERS, FLAG_UPPER_AOI_TRIGGER, TriggerId, TriggerTarget,
};
use crate::cellapp_real_entity::ScriptCall;
use crate::cellapp_space::{CellApp, Entity};
use crate::cellappmgr::SpaceId;

/// `EntityCache::MAX_LOD_LEVELS` — "TODO: Remove this restriction".
pub const MAX_LOD_LEVELS: usize = 4;

/// `NO_ID_ALIAS`: the alias meaning "use the full entity id".
pub const NO_ID_ALIAS: u8 = 0xff;

/// `BW_MAX_PRIORITY`, the `priority( Priority )` assertion bound.
pub const BW_MAX_PRIORITY: f64 = f64::MAX;

/// `UDP_OVERHEAD` (`lib/network/basictypes.hpp`), subtracted by
/// `ServerUtil::bpsToPacketSize`.
pub const UDP_OVERHEAD: i32 = 28;

/// `EntityCache::VehicleChangeNum`: the offload-streamed vehicle state.
pub const VEHICLE_CHANGE_NUM_OLD: u8 = 0;
/// The cache agrees with the entity and the entity has a vehicle.
pub const VEHICLE_CHANGE_NUM_HAS_VEHICLE: u8 = 1;
/// The cache agrees with the entity and the entity has no vehicle.
pub const VEHICLE_CHANGE_NUM_HAS_NO_VEHICLE: u8 = 2;

/// `EntityCache`'s private flag bits (`entity_cache.hpp`).
pub const ENTER_PENDING: u16 = 1 << 0;
/// Expecting `requestEntityUpdate` from the client.
pub const REQUEST_PENDING: u16 = 1 << 1;
/// Waiting to send `createEntity` to the client.
pub const CREATE_PENDING: u16 = 1 << 2;
/// Waiting to be removed from the priority queue.
pub const GONE: u16 = 1 << 3;
/// Do not send to the client.
pub const WITHHELD: u16 = 1 << 4;
/// Waiting to be removed and re-added to the AoI.
pub const REFRESH: u16 = 1 << 5;
/// Should be sent in the next `Witness::update`.
pub const PRIORITISED: u16 = 1 << 6;
/// The entity was added to the AoI by hand.
pub const MANUALLY_ADDED: u16 = 1 << 7;
/// The entity was added to the AoI by the trigger.
pub const ADDED_BY_TRIGGER: u16 = 1 << 8;
/// Volatile position updates are detailed at all distances.
pub const IS_ALWAYS_DETAILED: u16 = 1 << 9;

/// Any of these block a normal update in the send loop.
pub const NOT_UPDATABLE: u16 =
    ENTER_PENDING | REQUEST_PENDING | CREATE_PENDING | GONE | WITHHELD | REFRESH;

/// The client-related flags `onEntityRemovedFromClient` clears.
pub const CLIENT_STATE: u16 = ENTER_PENDING | REQUEST_PENDING | CREATE_PENDING | GONE | REFRESH;

/// `EventNumber`.
pub type EventNumber = i32;
/// `VolatileNumber`.
pub type VolatileNumber = u16;
/// `DetailLevel`.
pub type DetailLevel = u8;
/// `AoIUpdateSchemeID`.
pub type SchemeId = u8;

/// One client-server property of one entity: its detail level and the
/// event stamp it last changed at. The port's stand-in for
/// `EntityDescription::clientServerProperty` +
/// `Entity::propertyEventStamps().get`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ClientProperty {
    /// `DataDescription::detailLevel()`.
    pub level: DetailLevel,
    /// The stamp `propertyEventStamps().get( *pDataDesc )` reads.
    pub stamp: EventNumber,
}

/// The per-type description faces the witness reads
/// (`EntityDescription::canBeOnClient`/`shouldSendDetailedPosition`/
/// `isManualAoI`/`aoiUpdateSchemeID` + `Entity::volatileInfo().
/// hasVolatile` + `description().lodLevels()`).
#[derive(Clone, Debug)]
pub struct EntityTypeDef {
    /// `canBeOnClient()`.
    pub can_be_on_client: bool,
    /// `shouldSendDetailedPosition()`: the `isAlwaysDetailed` default a
    /// fresh cache takes.
    pub should_send_detailed_position: bool,
    /// `isManualAoI()`.
    pub manual_aoi: bool,
    /// `Entity::volatileInfo().hasVolatile( 0.f )`: whether the entity
    /// may take an id alias and ride the non-detailed create message.
    pub has_volatile: bool,
    /// `EntityDescription::aoiUpdateSchemeID()`.
    pub update_scheme: SchemeId,
    /// `description().lodLevels()`: each level's `(start, hyst)` pair,
    /// in definition order (sorted by `start` like
    /// `DataLoDLevels::addLevels`).
    pub lod_levels: Vec<(f32, f32)>,
}

impl Default for EntityTypeDef {
    /// A type with none of the optional faces and the single outermost
    /// LoD level `DataLoDLevels`' constructor gives (`size_ = 1`,
    /// `start = FLT_MAX`, `hyst = 0`).
    fn default() -> Self {
        Self {
            can_be_on_client: true,
            should_send_detailed_position: false,
            manual_aoi: false,
            has_volatile: true,
            update_scheme: 0,
            lod_levels: Vec::new(),
        }
    }
}

/// `DataLoDLevel`: the `(low, high)` priority window of one level.
#[derive(Clone, Copy, Debug)]
pub struct LodLevel {
    /// Below this priority the level wants more detail.
    pub low: f32,
    /// Above this priority the level wants less detail.
    pub high: f32,
    /// The definition's `start` (the squared thresholds live in
    /// low/high).
    pub start: f32,
    /// The definition's `hyst`.
    pub hyst: f32,
}

/// `DataLoDLevels` (`lib/entitydef/data_lod_level.*`): the LoD
/// transition thresholds one entity type's properties hang off. The
/// `levels` vector holds the real levels at `0..size` and the untouched
/// `DataLoDLevel()` defaults at `size` — the reference's
/// `MAX_DATA_LOD_LEVELS + 1` array shape, whose sentinel makes the
/// initial walk down from `detailLevel_ = size` always descend once.
#[derive(Clone, Debug)]
pub struct LodLevels {
    /// The finalised thresholds, sentinel last.
    pub levels: Vec<LodLevel>,
}

impl LodLevels {
    /// `DataLoDLevels()`'s single outermost level, `start = FLT_MAX`.
    pub fn single() -> Self {
        Self {
            levels: vec![
                LodLevel {
                    low: -1.0,
                    high: f32::MAX,
                    start: f32::MAX,
                    hyst: 0.0,
                },
                LodLevel {
                    low: f32::MAX,
                    high: f32::MAX,
                    start: f32::MAX,
                    hyst: 0.0,
                },
            ],
        }
    }

    /// `DataLoDLevels::addLevels` + `finalise`: sort by `start`, then
    /// derive each window — `level[0].low = -1`, `level[i].low =
    /// prev.start²`, `level[i].high = (start + hyst)²` unless last
    /// (whose high stays `FLT_MAX`).
    pub fn from_starts(starts: &[(f32, f32)]) -> Self {
        let mut starts = starts.to_vec();
        starts.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
        if starts.is_empty() {
            return Self::single();
        }
        let size = starts.len();
        let mut levels = Vec::with_capacity(size + 1);
        for (index, &(start, hyst)) in starts.iter().enumerate() {
            let low = if index == 0 {
                -1.0
            } else {
                let v = starts[index - 1].0;
                v * v
            };
            let high = if index + 1 == size {
                f32::MAX
            } else {
                let v = start + hyst;
                v * v
            };
            levels.push(LodLevel {
                low,
                high,
                start,
                hyst,
            });
        }
        levels.push(LodLevel {
            low: f32::MAX,
            high: f32::MAX,
            start: f32::MAX,
            hyst: 0.0,
        });
        Self { levels }
    }

    /// `DataLoDLevels::size`.
    pub fn size(&self) -> usize {
        self.levels.len() - 1
    }

    /// `DataLoDLevels::needsMoreDetail`.
    pub fn needs_more_detail(&self, level: usize, priority: f32) -> bool {
        priority < self.levels[level].low
    }

    /// `DataLoDLevels::needsLessDetail`.
    pub fn needs_less_detail(&self, level: usize, priority: f32) -> bool {
        priority > self.levels[level].high
    }
}

/// `AoIUpdateScheme` (`aoi_update_schemes.hpp`): the priority delta one
/// distance maps to.
#[derive(Clone, Copy, Debug)]
pub struct AoiUpdateScheme {
    /// The flat weight (`minDelta`).
    pub weighting: f32,
    /// The distance weight (`(maxDelta/minDelta - 1) / maxAoIRadius`).
    pub distance_weighting: f32,
}

impl Default for AoiUpdateScheme {
    /// The reference's default constructor: `weighting 1,
    /// distanceWeighting 0.2` — the shape scheme 0 lands on with the
    /// 1/101 delta defaults and the 500 radius.
    fn default() -> Self {
        Self {
            weighting: 1.0,
            distance_weighting: 0.2,
        }
    }
}

impl AoiUpdateScheme {
    /// `shouldTreatAsCoincident`: both weights zero.
    pub fn should_treat_as_coincident(&self) -> bool {
        self.weighting == 0.0 && self.distance_weighting == 0.0
    }

    /// `apply`: the priority delta for `distance_squared`.
    pub fn apply(&self, distance_squared: f32) -> f64 {
        if self.should_treat_as_coincident() {
            return 1.0;
        }
        let distance = distance_squared.sqrt();
        (distance * self.distance_weighting + 1.0) as f64 * self.weighting as f64
    }

    /// `AoIUpdateScheme::init`: `minDelta` is the weight, the distance
    /// weight comes from the `maxDelta/minDelta` ratio spread over
    /// `maxAoIRadius`; both zero means the coincident scheme.
    pub fn init(
        &mut self,
        min_delta: f32,
        max_delta: f32,
        max_aoi_radius: f32,
    ) -> Result<(), &'static str> {
        if min_delta < 0.0 || max_delta < 0.0 {
            return Err("deltas must be non-negative");
        }
        if (min_delta == 0.0 || max_delta == 0.0) && min_delta + max_delta != 0.0 {
            return Err("both deltas must be zero or both positive");
        }
        if min_delta == 0.0 && max_delta == 0.0 {
            self.weighting = 0.0;
            self.distance_weighting = 0.0;
        } else {
            self.weighting = min_delta;
            self.distance_weighting = (max_delta / min_delta - 1.0) / max_aoi_radius;
        }
        Ok(())
    }
}

/// `AoIUpdateSchemes`: the named scheme table every cache points into.
#[derive(Clone, Debug)]
pub struct AoIUpdateSchemes {
    /// `schemes_[256]` — the entries a name maps to.
    pub schemes: BTreeMap<SchemeId, AoiUpdateScheme>,
    /// `nameToScheme_`.
    pub name_to_scheme: BTreeMap<String, SchemeId>,
    /// `schemeToName_`.
    pub scheme_to_name: BTreeMap<SchemeId, String>,
}

impl Default for AoIUpdateSchemes {
    /// `AoIUpdateSchemes::init`'s built-in half under the config
    /// defaults (`witnessUpdateDefaultMinDelta 1`,
    /// `witnessUpdateDefaultMaxDelta 101`, `maxAoIRadius 500`).
    fn default() -> Self {
        Self::with_default(1.0, 101.0, 500.0)
    }
}

impl AoIUpdateSchemes {
    /// `AoIUpdateSchemes::init`'s built-in half: the default scheme at 0
    /// under both the empty name and "default", shaped by the config's
    /// min/max delta pair.
    pub fn with_default(min_delta: f32, max_delta: f32, max_aoi_radius: f32) -> Self {
        let mut schemes = Self {
            schemes: BTreeMap::new(),
            name_to_scheme: BTreeMap::new(),
            scheme_to_name: BTreeMap::new(),
        };
        let mut scheme = AoiUpdateScheme::default();
        let _ = scheme.init(min_delta, max_delta, max_aoi_radius);
        schemes.schemes.insert(0, scheme);
        schemes.name_to_scheme.insert(String::new(), 0);
        schemes.name_to_scheme.insert("default".to_string(), 0);
        schemes.scheme_to_name.insert(0, "default".to_string());
        schemes
    }

    /// `AoIUpdateSchemes::init`'s registration half: custom schemes from
    /// id 1 up, refusing empty and duplicate names. Ids wrap at 256 —
    /// the "too many schemes defined" refusal.
    pub fn register(
        &mut self,
        name: &str,
        min_delta: f32,
        max_delta: f32,
        max_aoi_radius: f32,
    ) -> Result<SchemeId, &'static str> {
        if name.is_empty() {
            return Err("scheme name cannot be empty");
        }
        if self.name_to_scheme.contains_key(name) {
            return Err("duplicate scheme name");
        }
        let next = self
            .scheme_to_name
            .keys()
            .copied()
            .max()
            .unwrap_or(0)
            .wrapping_add(1);
        if next == 0 {
            return Err("too many schemes defined");
        }
        let mut scheme = AoiUpdateScheme::default();
        scheme.init(min_delta, max_delta, max_aoi_radius)?;
        self.schemes.insert(next, scheme);
        self.name_to_scheme.insert(name.to_string(), next);
        self.scheme_to_name.insert(next, name.to_string());
        Ok(next)
    }

    /// `AoIUpdateSchemes::shouldTreatAsCoincident`.
    pub fn should_treat_as_coincident(&self, scheme: SchemeId) -> bool {
        self.schemes
            .get(&scheme)
            .copied()
            .unwrap_or_default()
            .should_treat_as_coincident()
    }

    /// `AoIUpdateSchemes::apply`.
    pub fn apply(&self, scheme: SchemeId, distance_squared: f32) -> f64 {
        self.schemes
            .get(&scheme)
            .copied()
            .unwrap_or_default()
            .apply(distance_squared)
    }

    /// `AoIUpdateSchemes::getNameFromID`.
    pub fn get_name_from_id(&self, id: SchemeId) -> Option<&str> {
        self.scheme_to_name.get(&id).map(String::as_str)
    }

    /// `AoIUpdateSchemes::getIDFromName`.
    pub fn get_id_from_name(&self, name: &str) -> Option<SchemeId> {
        self.name_to_scheme.get(name).copied()
    }
}

/// One entry of `EntityCacheMap` (`entity_cache.hpp`): everything the
/// witness knows about one other entity.
#[derive(Clone, Debug)]
pub struct EntityCache {
    /// `pEntity_`: the id of the entity this cache watches.
    pub entity: u32,
    /// `flags_`.
    pub flags: u16,
    /// `updateSchemeID_`, taken from the entity type at construction.
    pub update_scheme_id: SchemeId,
    /// `vehicleChangeNum_`.
    pub vehicle_change_num: u8,
    /// `priority_`.
    pub priority: f64,
    /// `lastPriorityDelta_`: the throttle's denominator.
    pub last_priority_delta: f64,
    /// `lastEventNumber_`.
    pub last_event_number: EventNumber,
    /// `lastVolatileUpdateNumber_`.
    pub last_volatile_update_number: VolatileNumber,
    /// `detailLevel_`.
    pub detail_level: DetailLevel,
    /// `idAlias_`.
    pub id_alias: u8,
    /// `lodEventNumbers_`.
    pub lod_event_numbers: [EventNumber; MAX_LOD_LEVELS],
    /// `numLoDLevels()`, read off the type once — the reference
    /// recomputes it from the description every call.
    pub num_lod_levels: usize,
    /// `pEntity_->pVehicle()` at stream time: the offload face carries
    /// it.
    pub vehicle: Option<u32>,
}

impl EntityCache {
    /// `EntityCache::EntityCache` + `resetClientState`
    /// (`entity_cache.ipp`): the client starts having seen everything
    /// up to now and no detail at all.
    pub fn new(
        entity: u32,
        update_scheme_id: SchemeId,
        num_lod_levels: usize,
        last_event_number: EventNumber,
        volatile_update_number: VolatileNumber,
    ) -> Self {
        let mut cache = Self {
            entity,
            flags: 0,
            update_scheme_id,
            vehicle_change_num: VEHICLE_CHANGE_NUM_OLD,
            priority: 0.0,
            last_priority_delta: f64::MAX,
            last_event_number: 0,
            last_volatile_update_number: 0,
            detail_level: 0,
            id_alias: NO_ID_ALIAS,
            lod_event_numbers: [0; MAX_LOD_LEVELS],
            num_lod_levels,
            vehicle: None,
        };
        cache.reset_client_state(last_event_number, volatile_update_number);
        cache
    }

    /// `EntityCache::resetClientState`.
    pub fn reset_client_state(
        &mut self,
        last_event_number: EventNumber,
        volatile_update_number: VolatileNumber,
    ) {
        self.last_event_number = last_event_number;
        // `volatileUpdateNumber() - 1`: the client has seen none,
        // wrapping on zero like the reference's unsigned decrement.
        self.last_volatile_update_number = volatile_update_number.wrapping_sub(1);
        self.detail_level = self.num_lod_levels as DetailLevel;
        self.id_alias = NO_ID_ALIAS;
        // `DBL_MAX`: "the last delta was infinitely large and thus the
        // new delta should not be capped".
        self.last_priority_delta = f64::MAX;
        for stamp in self.lod_event_numbers[..self.num_lod_levels].iter_mut() {
            *stamp = 0;
        }
    }

    /// `EntityCache::onEntityRemovedFromClient`: the client state goes,
    /// the server state (withheld, manual adds) stays.
    pub fn on_entity_removed_from_client(
        &mut self,
        last_event_number: EventNumber,
        volatile_update_number: VolatileNumber,
    ) {
        self.flags &= !CLIENT_STATE;
        self.reset_client_state(last_event_number, volatile_update_number);
    }

    // ----- The flag faces --------------------------------------------

    /// `setEnterPending`.
    pub fn set_enter_pending(&mut self) {
        self.flags |= ENTER_PENDING;
    }
    /// `setRequestPending`.
    pub fn set_request_pending(&mut self) {
        self.flags |= REQUEST_PENDING;
    }
    /// `setCreatePending`.
    pub fn set_create_pending(&mut self) {
        self.flags |= CREATE_PENDING;
    }
    /// `setGone`.
    pub fn set_gone(&mut self) {
        self.flags |= GONE;
    }
    /// `setWithheld`.
    pub fn set_withheld(&mut self) {
        self.flags |= WITHHELD;
    }
    /// `setRefresh`.
    pub fn set_refresh(&mut self) {
        self.flags |= REFRESH;
    }
    /// `setPrioritised`.
    pub fn set_prioritised(&mut self) {
        self.flags |= PRIORITISED;
    }
    /// `setManuallyAdded`.
    pub fn set_manually_added(&mut self) {
        self.flags |= MANUALLY_ADDED;
    }
    /// `setAddedByTrigger`.
    pub fn set_added_by_trigger(&mut self) {
        self.flags |= ADDED_BY_TRIGGER;
    }
    /// `setAlwaysDetailed( bool )`.
    pub fn set_always_detailed(&mut self, value: bool) {
        if value {
            self.flags |= IS_ALWAYS_DETAILED;
        } else {
            self.flags &= !IS_ALWAYS_DETAILED;
        }
    }

    /// `clearEnterPending`.
    pub fn clear_enter_pending(&mut self) {
        self.flags &= !ENTER_PENDING;
    }
    /// `clearRequestPending`.
    pub fn clear_request_pending(&mut self) {
        self.flags &= !REQUEST_PENDING;
    }
    /// `clearCreatePending`.
    pub fn clear_create_pending(&mut self) {
        self.flags &= !CREATE_PENDING;
    }
    /// `clearGone`.
    pub fn clear_gone(&mut self) {
        self.flags &= !GONE;
    }
    /// `clearWithheld`.
    pub fn clear_withheld(&mut self) {
        self.flags &= !WITHHELD;
    }
    /// `clearRefresh`.
    pub fn clear_refresh(&mut self) {
        self.flags &= !REFRESH;
    }
    /// `clearPrioritised`.
    pub fn clear_prioritised(&mut self) {
        self.flags &= !PRIORITISED;
    }
    /// `clearManuallyAdded`.
    pub fn clear_manually_added(&mut self) {
        self.flags &= !MANUALLY_ADDED;
    }
    /// `clearAddedByTrigger`.
    pub fn clear_added_by_trigger(&mut self) {
        self.flags &= !ADDED_BY_TRIGGER;
    }

    /// `isEnterPending`.
    pub fn is_enter_pending(&self) -> bool {
        self.flags & ENTER_PENDING != 0
    }
    /// `isRequestPending`.
    pub fn is_request_pending(&self) -> bool {
        self.flags & REQUEST_PENDING != 0
    }
    /// `isCreatePending`.
    pub fn is_create_pending(&self) -> bool {
        self.flags & CREATE_PENDING != 0
    }
    /// `isGone`.
    pub fn is_gone(&self) -> bool {
        self.flags & GONE != 0
    }
    /// `isWithheld`.
    pub fn is_withheld(&self) -> bool {
        self.flags & WITHHELD != 0
    }
    /// `isRefresh`.
    pub fn is_refresh(&self) -> bool {
        self.flags & REFRESH != 0
    }
    /// `isPrioritised`.
    pub fn is_prioritised(&self) -> bool {
        self.flags & PRIORITISED != 0
    }
    /// `isUpdatable`: none of the `NOT_UPDATABLE` flags set.
    pub fn is_updatable(&self) -> bool {
        self.flags & NOT_UPDATABLE == 0
    }
    /// `isManuallyAdded`.
    pub fn is_manually_added(&self) -> bool {
        self.flags & MANUALLY_ADDED != 0
    }
    /// `isAddedByTrigger`.
    pub fn is_added_by_trigger(&self) -> bool {
        self.flags & ADDED_BY_TRIGGER != 0
    }
    /// `isAlwaysDetailed`.
    pub fn is_always_detailed(&self) -> bool {
        self.flags & IS_ALWAYS_DETAILED != 0
    }

    // ----- The priority faces ----------------------------------------

    /// `EntityCache::getDistanceSquared` (`entity_cache.ipp`), the
    /// `FLT_MAX` clamp included.
    pub fn distance_squared(&self, origin: [f32; 3], entity: &Entity) -> f32 {
        let diff_x = entity.position[0] - origin[0];
        let diff_z = entity.position[2] - origin[2];
        let ret = diff_x * diff_x + diff_z * diff_z;
        // "FLT_MAX squared results in +inf; fix that".
        if ret > f32::MAX { f32::MAX } else { ret }
    }

    /// `EntityCache::getLoDPriority`: coincident schemes pin zero.
    pub fn lod_priority(
        &self,
        schemes: &AoIUpdateSchemes,
        origin: [f32; 3],
        entity: &Entity,
    ) -> f32 {
        if schemes.should_treat_as_coincident(self.update_scheme_id) {
            0.0
        } else {
            self.distance_squared(origin, entity)
        }
    }

    /// `EntityCache::updatePriority`: apply the scheme to the distance,
    /// cap the growth at a fraction of the last delta, accumulate.
    pub fn update_priority(
        &mut self,
        schemes: &AoIUpdateSchemes,
        delta_growth_throttle: f32,
        origin: [f32; 3],
        entity: &Entity,
    ) {
        let dist_sq = self.distance_squared(origin, entity);
        let mut delta = schemes.apply(self.update_scheme_id, dist_sq);
        let throttle = delta_growth_throttle as f64;
        if delta > throttle * self.last_priority_delta {
            delta = throttle * self.last_priority_delta;
        }
        self.last_priority_delta = delta;
        self.priority += delta;
    }

    /// `EntityCache::priority( Priority )` — the `MF_ASSERT` guard.
    pub fn set_priority(&mut self, new_priority: f64) {
        debug_assert!(new_priority.abs() < BW_MAX_PRIORITY);
        self.priority = new_priority;
    }

    // ----- The LoD faces ---------------------------------------------

    /// `EntityCache::lodEventNumber( level, eventNumber )`.
    pub fn set_lod_event_number(&mut self, level: usize, event_number: EventNumber) {
        debug_assert!(level < self.num_lod_levels);
        self.lod_event_numbers[level] = event_number;
    }

    /// `EntityCache::lodEventNumber( level )`.
    pub fn lod_event_number(&self, level: usize) -> EventNumber {
        debug_assert!(level < self.num_lod_levels);
        self.lod_event_numbers[level]
    }

    /// `EntityCache::lodEventNumbers`: the top `stamps.len()` levels
    /// take the input in order.
    pub fn set_lod_event_numbers(&mut self, stamps: &[EventNumber]) {
        let offset = self.num_lod_levels - stamps.len();
        for (index, stamp) in stamps.iter().enumerate() {
            self.lod_event_numbers[index + offset] = *stamp;
        }
    }

    /// The properties owing an update at the current detail level: the
    /// indices and whether there are any.
    fn changed_property_indices(&self, ctx: &WitnessContext<'_>) -> Vec<u8> {
        let lod_event_number = self.lod_event_number(self.detail_level as usize);
        let properties = ctx
            .properties
            .get(&self.entity)
            .cloned()
            .unwrap_or_default();
        let mut indices = Vec::new();
        for (index, property) in properties.iter().enumerate() {
            if property.level == self.detail_level && property.stamp > lod_event_number {
                indices.push(index as u8);
            }
        }
        indices
    }

    /// `EntityCache::reuse`: a re-entry before the removal ran. A cache
    /// whose client lags behind the entity's trimmed history cannot be
    /// reused — it is marked for the delete-and-re-enter refresh
    /// instead.
    pub fn reuse(&mut self, entity: &Entity, notes: &mut Vec<WitnessNote>) {
        debug_assert!(self.is_gone());
        self.clear_gone();
        if self.last_event_number < entity.last_trimmed_event_number as EventNumber
            && self.is_updatable()
        {
            notes.push(WitnessNote::ReuseMissedEvents {
                witness: self.entity,
                last_received: self.last_event_number,
                entity_at: entity.last_event_number as EventNumber,
                history_since: entity.last_trimmed_event_number as EventNumber,
            });
            self.set_refresh();
        }
    }

    /// `EntityCache::addEntitySelectMessage`: name the entity the next
    /// history events belong to, by alias when it has one.
    pub fn add_entity_select_message(&self, wire: &mut Vec<Wire>) {
        debug_assert!(self.is_updatable());
        if self.id_alias != NO_ID_ALIAS {
            wire.push(Wire::SelectAliasedEntity {
                id_alias: self.id_alias,
            });
        } else {
            wire.push(Wire::SelectEntity { id: self.entity });
        }
    }

    /// `EntityCache::addLeaveAoIMessage`: the id plus the LoD stamps —
    /// the levels below the current one send their stored stamps, the
    /// current and above all stamp the current event number.
    pub fn add_leave_aoi_message(&self, wire: &mut Vec<Wire>, id: u32) {
        let mut stamps = Vec::with_capacity(self.num_lod_levels);
        let below = self.detail_level as usize;
        stamps.extend_from_slice(&self.lod_event_numbers[..below]);
        for _ in below..self.num_lod_levels {
            stamps.push(self.last_event_number);
        }
        wire.push(Wire::LeaveAoI {
            id,
            lod_stamps: stamps,
        });
    }

    /// `EntityCache::addChangedProperties`: every client-server property
    /// at the current detail level stamped after the level's LoD stamp
    /// goes out as one update message. With a header, the select
    /// message precedes the `updateEntity` start. Returns whether
    /// anything was written.
    pub fn add_changed_properties(
        &mut self,
        ctx: &WitnessContext<'_>,
        wire: &mut Vec<Wire>,
        with_header: bool,
        should_select_entity: bool,
    ) -> bool {
        let indices = self.changed_property_indices(ctx);
        if indices.is_empty() {
            return false;
        }
        let num_to_send = indices.len();
        debug_assert!(num_to_send < 256);

        let mut selected = false;
        if with_header {
            selected = should_select_entity;
            if selected {
                self.add_entity_select_message(wire);
            }
            // `pBundleForHeader->startMessage( updateEntity )` — even
            // without the select.
        }
        wire.push(Wire::UpdateEntity {
            id: self.entity,
            num: num_to_send as u8,
            level: self.detail_level,
            indices,
            selected,
        });
        true
    }

    /// `EntityCache::updateDetailLevel`: walk the LoD windows against
    /// the priority, sending property catches-up on the way
    /// more-detailed and stamping the levels being left on the way
    /// less-detailed.
    pub fn update_detail_level(
        &mut self,
        ctx: &WitnessContext<'_>,
        wire: &mut Vec<Wire>,
        lod_levels: &LodLevels,
        lod_priority: f32,
        has_selected_entity: bool,
    ) -> bool {
        let mut has_written_to_stream = false;
        let mut has_selected = has_selected_entity;
        while lod_levels.needs_more_detail(self.detail_level as usize, lod_priority) {
            self.detail_level -= 1;
            let wrote = self.add_changed_properties(ctx, wire, true, !has_selected);
            has_written_to_stream |= wrote;
            has_selected |= wrote;
        }
        while lod_levels.needs_less_detail(self.detail_level as usize, lod_priority) {
            let level = self.detail_level as usize;
            let stamp = self.last_event_number;
            self.set_lod_event_number(level, stamp);
            self.detail_level += 1;
        }
        has_written_to_stream
    }

    /// `EntityCache::addOuterDetailLevel`: the create path — drop to
    /// the second-most-detailed level, restamp from the entity's own
    /// event number, and return the property indices that append raw
    /// into the create message.
    pub fn add_outer_detail_level(
        &mut self,
        ctx: &WitnessContext<'_>,
        last_event_number: EventNumber,
    ) -> Vec<u8> {
        debug_assert!(self.detail_level as usize == self.num_lod_levels);
        self.detail_level = self.num_lod_levels as DetailLevel - 1;
        self.last_event_number = last_event_number;
        self.changed_property_indices(ctx)
    }

    /// The streaming write half of `EntityCache::operator<<`: id first,
    /// then the cache fields, the vehicle state folded to its
    /// three-value enum, and the `size - 1` stamps the reader infers
    /// the rest from. The vehicle id rides only when the state says the
    /// entity has one.
    pub fn write_to_stream(&self, entity: &Entity) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&self.entity.to_le_bytes());
        out.extend_from_slice(&self.flags.to_le_bytes());
        out.push(self.update_scheme_id);
        let mut vehicle_state = VEHICLE_CHANGE_NUM_OLD;
        if entity.vehicle_change_num as u8 == self.vehicle_change_num {
            vehicle_state = if entity.vehicle.is_some() {
                VEHICLE_CHANGE_NUM_HAS_VEHICLE
            } else {
                VEHICLE_CHANGE_NUM_HAS_NO_VEHICLE
            };
        }
        out.push(vehicle_state);
        out.extend_from_slice(&self.priority.to_le_bytes());
        out.extend_from_slice(&self.last_event_number.to_le_bytes());
        out.push(self.id_alias);
        out.push(self.detail_level);
        out.push(self.num_lod_levels as u8);
        let seeded = self.num_lod_levels.saturating_sub(1);
        for stamp in &self.lod_event_numbers[..seeded] {
            out.extend_from_slice(&stamp.to_le_bytes());
        }
        if vehicle_state == VEHICLE_CHANGE_NUM_HAS_VEHICLE {
            // "This is read off in Witness::Witness."
            out.extend_from_slice(&entity.vehicle.unwrap_or(0).to_le_bytes());
        }
        out
    }
}

/// `EntityCacheMap` (`entity_cache.{hpp,cpp}`): the AoI's cache set.
/// The reference keeps a `BW::set` ordered by pointer identity; the
/// port's `BTreeMap` orders by entity id, which only reorders the
/// offload stream.
#[derive(Debug, Default)]
pub struct EntityCacheMap {
    /// The caches, by the id of the entity they watch.
    pub caches: BTreeMap<u32, EntityCache>,
    /// `g_numInAoI`.
    pub num_in_aoi: u32,
    /// `g_numInAoIEver`.
    pub num_in_aoi_ever: u32,
}

impl EntityCacheMap {
    /// `EntityCacheMap::add`.
    pub fn add(&mut self, cache: EntityCache) {
        self.num_in_aoi += 1;
        self.num_in_aoi_ever += 1;
        self.caches.insert(cache.entity, cache);
    }

    /// `EntityCacheMap::del`.
    pub fn del(&mut self, id: u32) {
        if self.caches.remove(&id).is_some() {
            self.num_in_aoi -= 1;
        }
    }

    /// `EntityCacheMap::find( const Entity & )`.
    pub fn find(&self, id: u32) -> Option<&EntityCache> {
        self.caches.get(&id)
    }

    /// `EntityCacheMap::find`'s mutable face.
    pub fn find_mut(&mut self, id: u32) -> Option<&mut EntityCache> {
        self.caches.get_mut(&id)
    }

    /// `EntityCacheMap::size`.
    pub fn size(&self) -> u32 {
        self.caches.len() as u32
    }
}

/// The client-message events a witness produces, in bundle order. The
/// port's stand-in for the `Mercury::Bundle` the reference appends to.
#[derive(Clone, Debug, PartialEq)]
pub enum Wire {
    /// `BaseAppIntInterface::selectPlayerEntity`.
    SelectPlayer,
    /// `selectEntityArgs`.
    SelectEntity {
        /// The entity to select.
        id: u32,
    },
    /// `selectAliasedEntityArgs`.
    SelectAliasedEntity {
        /// The alias to select by.
        id_alias: u8,
    },
    /// `enterAoIArgs`.
    EnterAoI {
        /// The entering entity.
        id: u32,
        /// The alias handed out for it.
        id_alias: u8,
    },
    /// `enterAoIOnVehicleArgs`.
    EnterAoIOnVehicle {
        /// The entering entity.
        id: u32,
        /// The vehicle it rides.
        vehicle_id: u32,
        /// The alias handed out for it.
        id_alias: u8,
    },
    /// `setVehicleArgs` (`Entity::writeVehicleChangeToBundle`).
    SetVehicle {
        /// The passenger whose vehicle changed.
        passenger_id: u32,
        /// Its new vehicle (zero when it alighted).
        vehicle_id: u32,
        /// The `vehicleChangeNum` the client is now at.
        change_num: u32,
    },
    /// `createEntity`/`createEntityDetailed` — detailed when the entity
    /// has no volatile info. The outer detail level's properties ride
    /// inside the create message (`addOuterDetailLevel`), recorded
    /// here as their indices.
    CreateEntity {
        /// The created entity.
        id: u32,
        /// Whether the detailed variant went out.
        detailed: bool,
        /// The detail level the create dropped to.
        detail_level: DetailLevel,
        /// The outer-level property indices in the payload.
        property_indices: Vec<u8>,
    },
    /// `updateEntity` (`EntityCache::addChangedProperties`).
    UpdateEntity {
        /// The entity the properties belong to.
        id: u32,
        /// The property count byte.
        num: u8,
        /// The detail level the update serves.
        level: DetailLevel,
        /// The property indices, in the description's order.
        indices: Vec<u8>,
        /// Whether a select message preceded it.
        selected: bool,
    },
    /// `leaveAoI`.
    LeaveAoI {
        /// The leaving entity.
        id: u32,
        /// The LoD stamps, one per level.
        lod_stamps: Vec<EventNumber>,
    },
    /// `sendMessageToClient` (`Witness::sendToClient`).
    SendMessageToClient {
        /// The entity the payload is addressed to.
        entity_id: u32,
        /// The client message id.
        message_id: u16,
        /// The payload size.
        size: i16,
    },
    /// `tickSync` (`Witness::Witness`'s opener).
    TickSync {
        /// The tick byte.
        tick_byte: u8,
    },
}

/// The WARNING/ERROR/INFO faces the witness records, one variant per
/// reference message this port produces.
#[derive(Clone, Debug, PartialEq)]
pub enum WitnessNote {
    /// `setAoIRadius`'s clamp to `maxAoIRadius`.
    RadiusClamped {
        /// The witnessing entity.
        id: u32,
        /// The radius asked for.
        radius: f32,
        /// The maximum allowed.
        max: f32,
    },
    /// `sendToClient`'s ERROR face: the target is outside the AoI.
    SelectOutsideAoI {
        /// The witness.
        witness: u32,
        /// The entity asked for.
        target: u32,
    },
    /// `requestEntityUpdate`'s DEBUG face: the entity is gone already.
    PendingNoLongerInAoI {
        /// The witness.
        witness: u32,
        /// The entity the request named.
        entity: u32,
    },
    /// `requestEntityUpdate`'s ERROR face: the entity asked for itself.
    RequestForSelf {
        /// The witness.
        witness: u32,
    },
    /// `requestEntityUpdate`'s ERROR face: nothing is pending.
    RequestNotPending {
        /// The witness.
        witness: u32,
        /// The entity the request named.
        entity: u32,
    },
    /// `requestEntityUpdate`'s CHEAT clamp.
    StampsBeyondLevels {
        /// The witness.
        witness: u32,
        /// The stamp count sent.
        size: usize,
        /// The level count allowed.
        max: usize,
    },
    /// `removeFromAoI`'s CRITICAL face.
    RemoveNotInAoI {
        /// The entity asked to leave.
        entity: u32,
        /// The witness it never joined.
        witness: u32,
    },
    /// `addToManualAoI`'s destroyed-entity ValueError face.
    AddDestroyedEntity {
        /// The destroyed entity.
        entity: u32,
    },
    /// `addToManualAoI`'s already-present ValueError face.
    AlreadyInAoI {
        /// The entity already present.
        entity: u32,
    },
    /// `removeFromManualAoI`'s gone-or-absent TypeError face.
    RemoveNotInManualAoI {
        /// The entity asked out.
        entity: u32,
    },
    /// `removeFromManualAoI`'s not-manual TypeError face.
    NotManualAoI {
        /// The entity asked out.
        entity: u32,
    },
    /// `updateManualAoI`'s WARNING face for a non-manual type with
    /// `ignoreNonManualAoIEntities`.
    NotManualAoIType {
        /// The entity in the list.
        entity: u32,
    },
    /// `Witness::update`'s ERROR face: the mount is outside the AoI.
    PrioritiseOutsideAoI {
        /// The witness.
        witness: u32,
        /// The mount the chain named.
        entity: u32,
    },
    /// `sendQueueElement`'s DEBUG_BANDWIDTH WARNING face.
    UpdateSpam {
        /// The entity whose update went out.
        entity: u32,
        /// The bytes it added.
        added: i32,
        /// The witness it went to.
        witness: u32,
    },
    /// `Witness::update`'s deficit WARNING face.
    DeficitCapped {
        /// The witness.
        witness: u32,
        /// The deficit before the cap.
        deficit: i32,
        /// The cap (`maxPacketSize_`).
        max: i32,
    },
    /// `Witness::update`'s underflow CRITICAL face.
    PriorityUnderflow {
        /// The witness.
        witness: u32,
        /// The queue-tail entity whose priority overflowed.
        entity: u32,
        /// The priority that failed `+0.0001`.
        priority: f64,
    },
    /// `EntityCache::reuse`'s INFO face: the client's history was
    /// trimmed out from under it, so the cache cannot be reused.
    ReuseMissedEvents {
        /// The entity the cache watches.
        witness: u32,
        /// `lastEventNumber()`.
        last_received: EventNumber,
        /// The entity's current event number.
        entity_at: EventNumber,
        /// `lastTrimmedEventNumber()`.
        history_since: EventNumber,
    },
}

/// The per-call context the witness faces borrow from the `CellApp`:
/// everything `Entity`/`EntityDescription` lookups in the reference go
/// through, split out so a witness in the map and the population can be
/// reached in one call.
pub struct WitnessContext<'a> {
    /// `CellApp::instance().findEntity`.
    pub entities: &'a BTreeMap<u32, Rc<RefCell<Entity>>>,
    /// The per-type description faces.
    pub type_faces: &'a BTreeMap<u32, EntityTypeDef>,
    /// `propertyEventStamps()`, per entity.
    pub properties: &'a BTreeMap<u32, Vec<ClientProperty>>,
    /// `CellAppConfig::aoiUpdateSchemes()`.
    pub schemes: &'a AoIUpdateSchemes,
    /// `Entity::callback` deliveries.
    pub scripts: &'a mut Vec<ScriptCall>,
    /// The WARNING/INFO faces.
    pub warnings: &'a mut Vec<WitnessNote>,
    /// The ERROR/CRITICAL faces.
    pub errors: &'a mut Vec<WitnessNote>,
    /// `CellApp::emergencyThrottle()`: scales the desired packet size.
    pub throttle: f32,
    /// `CellAppConfig::witnessUpdateMaxPriorityDelta()`: the most
    /// priority change one pass walks past.
    pub max_priority_delta: f32,
    /// `Entity::writeClientUpdateDataToBundle`'s byte growth per send —
    /// the writer is BW-08-03-04's face; the selection loop only
    /// spends its size.
    pub update_bytes: i32,
    /// `CellAppConfig::entitySpamSize()`: the per-send size the spam
    /// WARNING guards.
    pub spam_size: i32,
    /// `CellAppConfig::witnessUpdateDeltaGrowthThrottle()`.
    pub delta_growth_throttle: f32,
}

impl<'a> WitnessContext<'a> {
    /// The type face of `entity`'s type, or the defaults.
    pub fn type_face(&self, entity: &Entity) -> EntityTypeDef {
        self.type_faces
            .get(&entity.type_id)
            .cloned()
            .unwrap_or_default()
    }

    /// `Entity::callback`: the AoI script hooks the witness fires.
    pub fn callback(&mut self, entity: u32, name: &'static str) {
        self.scripts.push(ScriptCall {
            entity,
            name,
            timer: None,
        });
    }
}

/// `Witness` (`witness.cpp`): the AoI one client sees, its send queue,
/// its alias pool and its bandwidth bookkeeping.
#[derive(Debug)]
pub struct Witness {
    /// `entity_`: the witnessing entity.
    pub entity_id: u32,
    /// The space whose range list the AoI trigger lives on.
    pub space_id: SpaceId,
    /// `aoiMap_`.
    pub aoi_map: EntityCacheMap,
    /// `entityQueue_`: cache ids in a min-heap by `priority()`.
    pub entity_queue: Vec<u32>,
    /// `freeAliases_` + `numFreeAliases_`: the alias pool, free ids
    /// packed at the front.
    pub free_aliases: [u8; 256],
    /// `numFreeAliases_`.
    pub num_free_aliases: u8,
    /// `aoiRadius_`.
    pub aoi_radius: f32,
    /// `aoiHyst_`.
    pub aoi_hyst: f32,
    /// `pAoITrigger_`.
    pub aoi_trigger: Option<TriggerId>,
    /// `maxPacketSize_`.
    pub max_packet_size: i32,
    /// `bandwidthDeficit_`.
    pub bandwidth_deficit: i32,
    /// `stealthFactor_`: the value the next `makeNoise` carries.
    pub stealth_factor: f32,
    /// `noiseCheckTime_`.
    pub noise_check_time: u32,
    /// `noisePropagatedTime_`.
    pub noise_propagated_time: u32,
    /// `noiseMade_`.
    pub noise_made: bool,
    /// The noise `makeNoise` deliveries: `(stealthFactor, category)`.
    pub noises: Vec<(f32, u8)>,
    /// `knownSpaceDataSeq_`: the first space-data sequence the client
    /// has not seen; `-1` until the space-data face runs.
    pub known_space_data_seq: i32,
    /// The bundle events, in the order the reference would send them.
    pub wire: Vec<Wire>,
    /// The crossing queue the AoI trigger appends to — shared with the
    /// trigger's `TriggerTarget::AoI`.
    pub aoi_events: Rc<RefCell<Vec<(u32, bool)>>>,
    /// `entity().isInAoIOffload()` on the witnessing entity: the
    /// destroyed-entity trigger bookkeeping. A matching removal is
    /// ignored and the flag cleared.
    pub in_aoi_offload: bool,
    /// `lastSentReliableGameTime_`.
    pub last_sent_reliable_game_time: u32,
}

impl Witness {
    /// `Witness::Witness` up to `readOffloadData`'s
    /// CREATE_REAL_FROM_INIT defaults, with the `freeAliases_`
    /// compaction done: every id is free except `NO_ID_ALIAS`, which is
    /// reserved.
    pub fn new(entity_id: u32, space_id: SpaceId, default_aoi_radius: f32) -> Self {
        let mut free_aliases = [0u8; 256];
        // "we first fill the array with 1s": every id but the reserved
        // one, packed at the front — the compaction loop's result.
        let mut num_free_aliases = 0u8;
        for index in 0..=u8::MAX {
            if index != NO_ID_ALIAS {
                free_aliases[num_free_aliases as usize] = index;
                num_free_aliases += 1;
            }
        }
        Self {
            entity_id,
            space_id,
            aoi_map: EntityCacheMap::default(),
            entity_queue: Vec::new(),
            free_aliases,
            num_free_aliases,
            aoi_radius: default_aoi_radius,
            aoi_hyst: 5.0,
            aoi_trigger: None,
            max_packet_size: 0,
            bandwidth_deficit: 0,
            stealth_factor: 0.0,
            noise_check_time: 0,
            noise_propagated_time: 0,
            noise_made: false,
            noises: Vec::new(),
            known_space_data_seq: -1,
            wire: vec![Wire::TickSync { tick_byte: 0 }],
            aoi_events: Rc::new(RefCell::new(Vec::new())),
            in_aoi_offload: false,
            last_sent_reliable_game_time: 0,
        }
    }

    /// The AoI crossing queue the trigger writes into.
    pub fn take_aoi_events(&self) -> Vec<(u32, bool)> {
        std::mem::take(&mut *self.aoi_events.borrow_mut())
    }

    // ----- The heap ---------------------------------------------------

    /// The live priority of a queued id.
    fn queue_priority(&self, id: u32) -> f64 {
        self.aoi_map
            .caches
            .get(&id)
            .map(|cache| cache.priority)
            .unwrap_or(0.0)
    }

    /// `std::push_heap` under `PriorityCompare`: append and bubble the
    /// new id toward the front while it beats its parent's priority.
    pub fn push_seen(&mut self, id: u32) {
        self.entity_queue.push(id);
        let mut child = self.entity_queue.len() - 1;
        while child > 0 {
            let parent = (child - 1) / 2;
            if self.queue_priority(self.entity_queue[child])
                >= self.queue_priority(self.entity_queue[parent])
            {
                break;
            }
            self.entity_queue.swap(child, parent);
            child = parent;
        }
    }

    /// `std::pop_heap` + `pop_back`: the front id leaves, the last id
    /// takes its place and sifts down. `PriorityCompare` makes this a
    /// min-heap — the front is the lowest priority.
    pub fn pop_seen(&mut self) -> Option<u32> {
        if self.entity_queue.is_empty() {
            return None;
        }
        let front = self.entity_queue[0];
        let last = self.entity_queue.pop().expect("the queue is not empty");
        if !self.entity_queue.is_empty() {
            self.entity_queue[0] = last;
            let len = self.entity_queue.len();
            let mut parent = 0;
            loop {
                let left = parent * 2 + 1;
                let right = parent * 2 + 2;
                let mut best = parent;
                if left < len
                    && self.queue_priority(self.entity_queue[left])
                        < self.queue_priority(self.entity_queue[best])
                {
                    best = left;
                }
                if right < len
                    && self.queue_priority(self.entity_queue[right])
                        < self.queue_priority(self.entity_queue[best])
                {
                    best = right;
                }
                if best == parent {
                    break;
                }
                self.entity_queue.swap(parent, best);
                parent = best;
            }
        }
        Some(front)
    }

    /// `Witness::addToSeen`: a new cache joins at the queue's current
    /// minimum priority so the next pass sends it first.
    pub fn add_to_seen(&mut self, entity_id: u32) {
        let initial_priority = self
            .entity_queue
            .first()
            .map(|&front| self.queue_priority(front))
            .unwrap_or(0.0);
        if let Some(cache) = self.aoi_map.caches.get_mut(&entity_id) {
            cache.set_priority(initial_priority);
        }
        self.push_seen(entity_id);
    }

    /// `std::make_heap`: restore the min-heap order after an arbitrary
    /// queue edit.
    fn make_seen_heap(&mut self) {
        let len = self.entity_queue.len();
        for parent in (0..len / 2).rev() {
            let mut root = parent;
            loop {
                let left = root * 2 + 1;
                let right = root * 2 + 2;
                let mut best = root;
                if left < len
                    && self.queue_priority(self.entity_queue[left])
                        < self.queue_priority(self.entity_queue[best])
                {
                    best = left;
                }
                if right < len
                    && self.queue_priority(self.entity_queue[right])
                        < self.queue_priority(self.entity_queue[best])
                {
                    best = right;
                }
                if best == root {
                    break;
                }
                self.entity_queue.swap(root, best);
                root = best;
            }
        }
    }

    // ----- The AoI membership faces -----------------------------------

    /// `Witness::addToAoI` (`witness.cpp:2202`): first observation
    /// creates the cache enter-pending and queues it; a cache already
    /// known is either revived (`reuse`) or just re-flagged; a
    /// destroyed entity sets the offload flag its matching removal
    /// clears.
    pub fn add_to_aoi(
        &mut self,
        ctx: &mut WitnessContext<'_>,
        entity_rc: &Rc<RefCell<Entity>>,
        set_manually_added: bool,
    ) {
        if entity_rc.borrow().destroyed {
            debug_assert!(!self.in_aoi_offload);
            // "We use this flag to indicate that the current dead entity
            // has been triggered artificially."
            self.in_aoi_offload = true;
            return;
        }

        let face = ctx.type_face(&entity_rc.borrow());
        if !face.can_be_on_client {
            return;
        }

        let entity_id = entity_rc.borrow().id;
        let existing_gone = self.aoi_map.find(entity_id).map(|cache| cache.is_gone());
        if let Some(gone) = existing_gone {
            if gone {
                let cache = self
                    .aoi_map
                    .find_mut(entity_id)
                    .expect("the cache was just read");
                cache.reuse(&entity_rc.borrow(), ctx.warnings);
            } else {
                let cache = self
                    .aoi_map
                    .find_mut(entity_id)
                    .expect("the cache was just read");
                if set_manually_added {
                    cache.set_manually_added();
                } else {
                    cache.set_added_by_trigger();
                }
                return;
            }
        } else {
            let mut cache = self.make_cache(ctx, entity_rc);
            cache.set_enter_pending();
            let new_id = cache.entity;
            self.aoi_map.add(cache);
            self.add_to_seen(new_id);
        }

        let cache = self
            .aoi_map
            .find_mut(entity_id)
            .expect("the cache lives by here");
        if set_manually_added {
            cache.set_manually_added();
        } else {
            cache.set_added_by_trigger();
        }

        // "Don't re-call onEnteredAoI during an offload" — the offload
        // lane is deferred, so the entity is never in the offload state
        // here and the call is unconditional.
        let face = ctx.type_face(&entity_rc.borrow());
        let cache = self
            .aoi_map
            .find_mut(entity_id)
            .expect("the cache lives by here");
        cache.set_always_detailed(face.should_send_detailed_position);
        ctx.callback(self.entity_id, "onEnteredAoI");
    }

    /// `EntityCacheMap::add`'s construction half, split out so the
    /// caller can flag the cache before it lands.
    fn make_cache(&self, ctx: &WitnessContext<'_>, entity_rc: &Rc<RefCell<Entity>>) -> EntityCache {
        let entity = entity_rc.borrow();
        let face = ctx.type_face(&entity);
        let lod_levels = LodLevels::from_starts(&face.lod_levels).size();
        EntityCache::new(
            entity.id,
            face.update_scheme,
            lod_levels,
            entity.last_event_number as EventNumber,
            entity.volatile_update_number,
        )
    }

    /// `Witness::removeFromAoI` (`witness.cpp:2305`): mark gone and
    /// tell the script; the wire message waits for the send pass.
    pub fn remove_from_aoi(
        &mut self,
        ctx: &mut WitnessContext<'_>,
        entity_rc: &Rc<RefCell<Entity>>,
        clear_manually_added: bool,
    ) {
        // "Check if this is a remove that is matched with an ignored
        // addToAoI call."
        if self.in_aoi_offload {
            debug_assert!(entity_rc.borrow().destroyed);
            self.in_aoi_offload = false;
            return;
        }

        let face = ctx.type_face(&entity_rc.borrow());
        if !face.can_be_on_client {
            return;
        }

        let entity_id = entity_rc.borrow().id;
        let Some(cache) = self.aoi_map.find_mut(entity_id) else {
            ctx.errors.push(WitnessNote::RemoveNotInAoI {
                entity: entity_id,
                witness: self.entity_id,
            });
            return;
        };

        if clear_manually_added {
            cache.clear_manually_added();
            if cache.is_added_by_trigger() {
                return;
            }
        } else {
            cache.clear_added_by_trigger();
            if cache.is_manually_added() {
                return;
            }
        }

        debug_assert!(!cache.is_gone());
        cache.set_gone();
        ctx.callback(self.entity_id, "onLeftAoI");
    }

    // ----- The send-pass state machine --------------------------------

    /// `Witness::handleStateChange` (`witness.cpp:986`) for the popped
    /// cache: gone leaves the client and the AoI, withheld is made
    /// unknown, enter-pending sends, create-pending creates, refresh
    /// does the delete-and-re-enter dance.
    pub fn handle_state_change(&mut self, ctx: &mut WitnessContext<'_>, entity_id: u32) {
        let gone = self
            .aoi_map
            .find(entity_id)
            .map(|cache| cache.is_gone())
            .unwrap_or(false);
        if gone {
            let mut wire = std::mem::take(&mut self.wire);
            self.delete_from_client(ctx, &mut wire, entity_id);
            self.wire = wire;
            self.delete_from_aoi(entity_id);
            return;
        }
        let withheld = self
            .aoi_map
            .find(entity_id)
            .map(|cache| cache.is_withheld())
            .unwrap_or(false);
        if withheld {
            // "If client knows about a withheld entity, make it un-know."
            let enter_pending = self
                .aoi_map
                .find(entity_id)
                .map(|cache| cache.is_enter_pending())
                .unwrap_or(false);
            if !enter_pending {
                let mut wire = std::mem::take(&mut self.wire);
                self.delete_from_client(ctx, &mut wire, entity_id);
                self.wire = wire;
                debug_assert!(
                    self.aoi_map
                        .find(entity_id)
                        .map(|cache| cache.is_withheld())
                        .unwrap_or(false)
                );
                let cache = self
                    .aoi_map
                    .find_mut(entity_id)
                    .expect("the cache stays through the client reset");
                cache.set_enter_pending();
            }
            return;
        }
        let enter_pending = self
            .aoi_map
            .find(entity_id)
            .map(|cache| cache.is_enter_pending())
            .unwrap_or(false);
        if enter_pending {
            let mut wire = std::mem::take(&mut self.wire);
            let entity = ctx
                .entities
                .get(&entity_id)
                .cloned()
                .expect("a queued cache's entity lives");
            self.send_enter(ctx, &mut wire, &entity);
            self.wire = wire;
            // `handleEnterPending`: the cache left the queue with the
            // send — the caller popped it already.
            return;
        }
        let create_pending = self
            .aoi_map
            .find(entity_id)
            .map(|cache| cache.is_create_pending())
            .unwrap_or(false);
        if create_pending {
            let mut wire = std::mem::take(&mut self.wire);
            let entity = ctx
                .entities
                .get(&entity_id)
                .cloned()
                .expect("a queued cache's entity lives");
            self.send_create(ctx, &mut wire, &entity);
            self.wire = wire;
            let origin = self.origin(ctx);
            let cache = self.aoi_map.find_mut(entity_id).expect("the cache stays");
            cache.update_priority(ctx.schemes, 1.0 + 1.0 / 8.0, origin, &entity.borrow());
            return;
        }
        let refresh = self
            .aoi_map
            .find(entity_id)
            .map(|cache| cache.is_refresh())
            .unwrap_or(false);
        if refresh {
            let cache = self.aoi_map.find_mut(entity_id).expect("the cache stays");
            cache.clear_refresh();
            // "We are effectively resetting this entity cache's history,
            // tell the client that it's gone and (re-)entered."
            let mut wire = std::mem::take(&mut self.wire);
            self.delete_from_client(ctx, &mut wire, entity_id);
            let entity = ctx
                .entities
                .get(&entity_id)
                .cloned()
                .expect("a queued cache's entity lives");
            let cache = self.aoi_map.find_mut(entity_id).expect("the cache stays");
            cache.set_enter_pending();
            self.send_enter(ctx, &mut wire, &entity);
            self.wire = wire;
        }
    }

    /// The witness entity's position, the origin every priority uses.
    fn origin(&self, ctx: &WitnessContext<'_>) -> [f32; 3] {
        ctx.entities
            .get(&self.entity_id)
            .expect("the witnessed entity lives in the population")
            .borrow()
            .position
    }

    /// `Witness::sendEnter` (`witness.cpp:1601`): alias, the pending
    /// flip, the vehicle state, and the enter message.
    fn send_enter(
        &mut self,
        ctx: &mut WitnessContext<'_>,
        wire: &mut Vec<Wire>,
        entity_rc: &Rc<RefCell<Entity>>,
    ) {
        let entity_id = entity_rc.borrow().id;
        let alias = self.allocate_id_alias(ctx, &entity_rc.borrow());
        let cache = self
            .aoi_map
            .find_mut(entity_id)
            .expect("the enter-pending cache lives");
        cache.id_alias = alias;
        debug_assert!(!cache.is_request_pending());
        cache.clear_enter_pending();
        cache.set_request_pending();
        cache.vehicle_change_num = entity_rc.borrow().vehicle_change_num as u8;
        if let Some(vehicle_id) = entity_rc.borrow().vehicle {
            wire.push(Wire::EnterAoIOnVehicle {
                id: entity_id,
                vehicle_id,
                id_alias: alias,
            });
        } else {
            wire.push(Wire::EnterAoI {
                id: entity_id,
                id_alias: alias,
            });
        }
    }

    /// `Witness::allocateIDAlias` (`witness.cpp:1881`): only entities
    /// with volatile data take an alias, and only while the pool is
    /// not empty.
    fn allocate_id_alias(&mut self, ctx: &WitnessContext<'_>, entity: &Entity) -> u8 {
        let face = ctx.type_faces.get(&entity.type_id).cloned();
        if face.map(|f| f.has_volatile).unwrap_or(true) && self.num_free_aliases != 0 {
            self.num_free_aliases -= 1;
            return self.free_aliases[self.num_free_aliases as usize];
        }
        NO_ID_ALIAS
    }

    /// `Witness::sendCreate` (`witness.cpp:1643`): the vehicle change
    /// if the client's record is stale, then the create — the outer
    /// detail level's properties ride inside it.
    fn send_create(
        &mut self,
        ctx: &mut WitnessContext<'_>,
        wire: &mut Vec<Wire>,
        entity_rc: &Rc<RefCell<Entity>>,
    ) {
        let entity_id = entity_rc.borrow().id;
        let cache = self
            .aoi_map
            .find_mut(entity_id)
            .expect("the create-pending cache lives");
        cache.clear_create_pending();
        if cache.is_refresh() {
            // The reference's ERROR_MSG face.
            cache.clear_refresh();
        }
        debug_assert!(cache.is_updatable());

        self.write_vehicle_change(wire, entity_rc);

        let face = ctx.type_face(&entity_rc.borrow());
        let detailed = !face.has_volatile;
        let last_event_number = entity_rc.borrow().last_event_number as EventNumber;
        let (detail_level, property_indices) = {
            let cache = self
                .aoi_map
                .find_mut(entity_id)
                .expect("the create-pending cache lives");
            let indices = cache.add_outer_detail_level(ctx, last_event_number);
            (cache.detail_level, indices)
        };
        wire.push(Wire::CreateEntity {
            id: entity_id,
            detailed,
            detail_level,
            property_indices,
        });
    }

    /// `Entity::writeVehicleChangeToBundle` (`entity.cpp:2642`).
    fn write_vehicle_change(&mut self, wire: &mut Vec<Wire>, entity_rc: &Rc<RefCell<Entity>>) {
        let entity = entity_rc.borrow();
        let cache = self.aoi_map.find_mut(entity.id).expect("the cache lives");
        if cache.vehicle_change_num as u32 != entity.vehicle_change_num {
            wire.push(Wire::SetVehicle {
                passenger_id: entity.id,
                vehicle_id: entity.vehicle.unwrap_or(0),
                change_num: entity.vehicle_change_num,
            });
            cache.vehicle_change_num = entity.vehicle_change_num as u8;
        }
    }

    /// `Witness::deleteFromSeen`.
    pub fn delete_from_seen(
        &mut self,
        ctx: &mut WitnessContext<'_>,
        wire: &mut Vec<Wire>,
        entity_id: u32,
    ) {
        self.delete_from_client(ctx, wire, entity_id);
        self.delete_from_aoi(entity_id);
    }

    /// `Witness::deleteFromClient`: the leave message (unless the
    /// client never heard of it), the alias back to the pool, the
    /// client state reset.
    pub fn delete_from_client(
        &mut self,
        ctx: &mut WitnessContext<'_>,
        wire: &mut Vec<Wire>,
        entity_id: u32,
    ) {
        {
            let cache = self
                .aoi_map
                .find_mut(entity_id)
                .expect("the leaving cache lives");
            cache.clear_refresh();
            if !cache.is_enter_pending() {
                let cache = self
                    .aoi_map
                    .find(entity_id)
                    .expect("the leaving cache lives")
                    .clone();
                cache.add_leave_aoi_message(wire, entity_id);
            }
        }
        self.on_leave_aoi(entity_id);
        let entity = ctx
            .entities
            .get(&entity_id)
            .cloned()
            .expect("the leaving entity lives");
        let cache = self
            .aoi_map
            .find_mut(entity_id)
            .expect("the leaving cache lives");
        cache.on_entity_removed_from_client(
            entity.borrow().last_event_number as EventNumber,
            entity.borrow().volatile_update_number,
        );
    }

    /// `Witness::deleteFromAoI`: out of the queue and out of the map.
    pub fn delete_from_aoi(&mut self, entity_id: u32) {
        if let Some(index) = self
            .entity_queue
            .iter()
            .position(|&queued| queued == entity_id)
        {
            self.entity_queue.remove(index);
            self.make_seen_heap();
        }
        self.aoi_map.del(entity_id);
    }

    /// `Witness::onLeaveAoI`: the alias returns to the pool.
    pub fn on_leave_aoi(&mut self, entity_id: u32) {
        let Some(alias) = self.aoi_map.find(entity_id).map(|cache| cache.id_alias) else {
            return;
        };
        if alias != NO_ID_ALIAS {
            self.free_aliases[self.num_free_aliases as usize] = alias;
            self.num_free_aliases += 1;
        }
    }

    /// `Witness::requestEntityUpdate` (`witness.cpp:2025`): the
    /// client's ack moves the cache to create-pending and seeds the LoD
    /// stamps.
    pub fn request_entity_update(
        &mut self,
        ctx: &mut WitnessContext<'_>,
        entity_id: u32,
        stamps: &[EventNumber],
    ) {
        let Some(cache) = self.aoi_map.find_mut(entity_id) else {
            // "It should be rare that pEntity is NULL."
            if entity_id == self.entity_id {
                ctx.errors.push(WitnessNote::RequestForSelf {
                    witness: self.entity_id,
                });
            } else {
                ctx.warnings.push(WitnessNote::PendingNoLongerInAoI {
                    witness: self.entity_id,
                    entity: entity_id,
                });
            }
            return;
        };
        if !cache.is_request_pending() {
            ctx.errors.push(WitnessNote::RequestNotPending {
                witness: self.entity_id,
                entity: entity_id,
            });
            return;
        }
        cache.clear_request_pending();
        cache.set_create_pending();

        if stamps.len() > cache.num_lod_levels {
            ctx.errors.push(WitnessNote::StampsBeyondLevels {
                witness: self.entity_id,
                size: stamps.len(),
                max: cache.num_lod_levels,
            });
        }
        let taken = stamps.len().min(cache.num_lod_levels);
        cache.set_lod_event_numbers(&stamps[..taken]);
        self.add_to_seen(entity_id);
    }

    /// `Witness::update` (`witness.cpp:1088`): the per-tick send pass.
    /// The mount chain is prioritised first, then the heap walks by
    /// ascending priority while the priority change stays inside
    /// `max_priority_delta` and the bundle inside the throttled
    /// `desiredPacketSize` — popping runs the state machines on
    /// non-updatable caches and sends the updatable ones. The popped
    /// set re-heaps either fully drained (every priority resets) or
    /// clamped up to the earliest priority that stayed. Returns the
    /// send sequence, mounts first.
    pub fn update(&mut self, ctx: &mut WitnessContext<'_>) -> Vec<u32> {
        let mut sent = Vec::new();
        // The reference's writer faces (space data, the reference
        // position, the detailed player position) are BW-08-03-04's,
        // so the pass's bundle starts empty and every send spends
        // `update_bytes`.
        let mut bundle_bytes = 0i32;
        let desired = (self.max_packet_size as f32 * ctx.throttle) as i32 - self.bandwidth_deficit;
        let origin = self.origin(ctx);
        let starting_priority = self
            .entity_queue
            .first()
            .map(|&front| self.queue_priority(front))
            .unwrap_or(0.0);

        // The mount stack rides first, whatever its priority.
        let mut num_prioritised = 0i32;
        let mounts = self.vehicle_chain(ctx);
        for mount in mounts {
            let Some(cache) = self.aoi_map.find(mount) else {
                ctx.errors.push(WitnessNote::PrioritiseOutsideAoI {
                    witness: self.entity_id,
                    entity: mount,
                });
                continue;
            };
            let skip = !cache.is_updatable() || cache.is_request_pending();
            let destroyed = ctx
                .entities
                .get(&mount)
                .map(|entity| entity.borrow().destroyed)
                .unwrap_or(false);
            if skip || destroyed {
                continue;
            }
            debug_assert!(!cache.is_prioritised());
            self.aoi_map
                .find_mut(mount)
                .expect("the cache was just read")
                .set_prioritised();
            num_prioritised += 1;
            sent.push(mount);
            bundle_bytes += ctx.update_bytes;
            if ctx.update_bytes > ctx.spam_size {
                ctx.warnings.push(WitnessNote::UpdateSpam {
                    entity: mount,
                    added: ctx.update_bytes,
                    witness: self.entity_id,
                });
            }
        }

        // Things up to `maxPriority` change away from the front while
        // the packet still has space (the two bytes are the sequence
        // number).
        let max_priority = starting_priority + ctx.max_priority_delta as f64;
        let mut popped = Vec::new();
        while let Some(&front) = self.entity_queue.first() {
            if self.queue_priority(front) >= max_priority {
                break;
            }
            if bundle_bytes >= desired - 2 {
                break;
            }
            self.pop_seen();
            popped.push(front);
            let was_prioritised = self
                .aoi_map
                .find(front)
                .map(|cache| cache.is_prioritised())
                .unwrap_or(false);
            debug_assert!(
                !self
                    .aoi_map
                    .find(front)
                    .map(|cache| cache.is_request_pending())
                    .unwrap_or(false)
            );
            // A destroyed entity still held by hand leaves the AoI
            // here (`removeFromAoI` with `clearManuallyAdded`).
            let destroyed = ctx
                .entities
                .get(&front)
                .map(|entity| entity.borrow().destroyed)
                .unwrap_or(false);
            let gone = self
                .aoi_map
                .find(front)
                .map(|cache| cache.is_gone())
                .unwrap_or(false);
            if destroyed && !gone {
                debug_assert!(
                    self.aoi_map
                        .find(front)
                        .map(|cache| cache.is_manually_added())
                        .unwrap_or(false)
                );
                // The destroyed gate above resolved this same slot;
                // the lookup cannot miss now.
                let entity = ctx
                    .entities
                    .get(&front)
                    .cloned()
                    .expect("the destroyed gate found the entity");
                self.remove_from_aoi(ctx, &entity, true);
            }
            let face = self
                .aoi_map
                .find(front)
                .map(|cache| (cache.is_updatable(), cache.is_prioritised()));
            if let Some((updatable, prioritised)) = face {
                if !updatable {
                    self.handle_state_change(ctx, front);
                }
                if updatable && !prioritised {
                    // `sendQueueElement`: the writer is BW-08-03-04's
                    // face; the pass only spends its size.
                    bundle_bytes += ctx.update_bytes;
                    sent.push(front);
                    if ctx.update_bytes > ctx.spam_size {
                        ctx.warnings.push(WitnessNote::UpdateSpam {
                            entity: front,
                            added: ctx.update_bytes,
                            witness: self.entity_id,
                        });
                    }
                    let entity = ctx
                        .entities
                        .get(&front)
                        .cloned()
                        .expect("an updatable cache's entity lives");
                    self.aoi_map
                        .find_mut(front)
                        .expect("the cache stays")
                        .update_priority(
                            ctx.schemes,
                            ctx.delta_growth_throttle,
                            origin,
                            &entity.borrow(),
                        );
                }
            }
            if was_prioritised {
                // Prioritised mounts were sent before the loop; the
                // pop only re-seats them — within one pass no state
                // change can take a prioritised cache away.
                let cache = self
                    .aoi_map
                    .find_mut(front)
                    .expect("the prioritised cache is still here");
                cache.set_priority(starting_priority);
                let entity = ctx
                    .entities
                    .get(&front)
                    .cloned()
                    .expect("a prioritised cache's entity lives");
                cache.update_priority(
                    ctx.schemes,
                    ctx.delta_growth_throttle,
                    origin,
                    &entity.borrow(),
                );
                cache.clear_prioritised();
                num_prioritised -= 1;
                debug_assert!(num_prioritised >= 0);
            }
        }

        self.bandwidth_deficit = if bundle_bytes > desired {
            bundle_bytes - desired
        } else {
            0
        };
        if self.bandwidth_deficit > self.max_packet_size {
            ctx.warnings.push(WitnessNote::DeficitCapped {
                witness: self.entity_id,
                deficit: self.bandwidth_deficit,
                max: self.max_packet_size,
            });
            self.bandwidth_deficit = self.max_packet_size;
        }

        // The gone arm deleted its cache and the enter and refresh
        // arms left it request-pending — all three took the slot out
        // of the queue (the reference's `deleteFromAoI` swap and
        // `handleEnterPending`'s `pop_back`). Everything else rides
        // back on the heap.
        let carried: Vec<u32> = popped
            .into_iter()
            .filter(|&id| {
                self.aoi_map
                    .find(id)
                    .map(|cache| !cache.is_request_pending())
                    .unwrap_or(false)
            })
            .collect();
        if self.entity_queue.is_empty() {
            // Must have sent and popped all prioritised entities. The
            // queue-empty branch resets every popped priority so the
            // next pass sends in closeness order again.
            debug_assert!(num_prioritised == 0);
            for id in &carried {
                self.aoi_map
                    .find_mut(*id)
                    .expect("the carry check kept the cache")
                    .set_priority(0.0);
            }
            for id in &carried {
                self.push_seen(*id);
            }
            self.make_seen_heap();
        } else {
            let earliest_priority = self.queue_priority(self.entity_queue[0]);
            // Prioritised mounts the loop never popped lose the flag
            // here.
            for mount in self.vehicle_chain(ctx) {
                if num_prioritised == 0 {
                    break;
                }
                let Some(cache) = self.aoi_map.find_mut(mount) else {
                    continue;
                };
                if cache.is_prioritised() {
                    cache.clear_prioritised();
                    num_prioritised -= 1;
                    debug_assert!(num_prioritised >= 0);
                }
            }
            for id in carried {
                if self.queue_priority(id) < earliest_priority {
                    self.aoi_map
                        .find_mut(id)
                        .expect("the carry check kept the cache")
                        .set_priority(earliest_priority);
                }
                self.push_seen(id);
            }
        }

        // The floating-point underflow watch on the queue tail.
        if let Some(&back) = self.entity_queue.last() {
            let highest = self.queue_priority(back);
            if highest + 0.0001 <= highest {
                ctx.errors.push(WitnessNote::PriorityUnderflow {
                    witness: self.entity_id,
                    entity: back,
                    priority: highest,
                });
            }
        }

        sent
    }

    /// `entity_.pVehicle()`'s chain: the witness's mount, then its
    /// mount, and so on up.
    fn vehicle_chain(&self, ctx: &WitnessContext<'_>) -> Vec<u32> {
        let mut chain = Vec::new();
        let mut mount = ctx
            .entities
            .get(&self.entity_id)
            .map(|e| e.borrow().vehicle);
        while let Some(Some(id)) = mount {
            if chain.len() == 256 || chain.contains(&id) {
                break;
            }
            chain.push(id);
            mount = ctx.entities.get(&id).map(|e| e.borrow().vehicle);
        }
        chain
    }

    // ----- The query faces --------------------------------------------

    /// `Witness::isInAoI`: present and not gone.
    pub fn is_in_aoi(&self, entity_id: u32) -> bool {
        self.aoi_map
            .find(entity_id)
            .map(|cache| !cache.is_gone())
            .unwrap_or(false)
    }

    /// `Witness::entitiesInAoI`'s include-all face: every cache still
    /// in the map.
    pub fn entities_in_aoi(&self) -> Vec<u32> {
        self.aoi_map.caches.keys().copied().collect()
    }

    /// `Witness::entitiesInAoI( false, false )`: only the entities the
    /// client currently sees (not gone, not pending).
    pub fn entities_visible_to_client(&self) -> Vec<u32> {
        self.aoi_map
            .caches
            .values()
            .filter(|cache| cache.is_updatable())
            .map(|cache| cache.entity)
            .collect()
    }

    /// `Witness::entitiesInAoI`'s withheld-only face.
    pub fn entities_withheld(&self) -> Vec<u32> {
        self.aoi_map
            .caches
            .values()
            .filter(|cache| cache.is_withheld())
            .map(|cache| cache.entity)
            .collect()
    }

    /// `Witness::withholdFromClient` (`witness.cpp:2974`).
    pub fn withhold_from_client(&mut self, entity_id: u32, is_withheld: bool) -> bool {
        let Some(cache) = self.aoi_map.find_mut(entity_id) else {
            return false;
        };
        if is_withheld {
            cache.set_withheld();
        } else {
            cache.clear_withheld();
        }
        true
    }

    /// `Witness::setPositionDetailed` (`witness.cpp:3264`).
    pub fn set_position_detailed(&mut self, entity_id: u32, is_detailed: bool) -> bool {
        let Some(cache) = self.aoi_map.find_mut(entity_id) else {
            return false;
        };
        cache.set_always_detailed(is_detailed);
        true
    }

    /// `Witness::addToManualAoI` (`witness.cpp:3182`).
    pub fn add_to_manual_aoi(
        &mut self,
        ctx: &mut WitnessContext<'_>,
        entity_rc: &Rc<RefCell<Entity>>,
    ) -> bool {
        if entity_rc.borrow().destroyed {
            ctx.warnings.push(WitnessNote::AddDestroyedEntity {
                entity: entity_rc.borrow().id,
            });
            return false;
        }
        let entity_id = entity_rc.borrow().id;
        let known = self
            .aoi_map
            .find(entity_id)
            .map(|cache| !cache.is_gone() && cache.is_manually_added())
            .unwrap_or(false);
        if known {
            ctx.warnings
                .push(WitnessNote::AlreadyInAoI { entity: entity_id });
            return false;
        }
        self.add_to_aoi(ctx, entity_rc, true);
        true
    }

    /// `Witness::removeFromManualAoI` (`witness.cpp:3227`).
    pub fn remove_from_manual_aoi(
        &mut self,
        ctx: &mut WitnessContext<'_>,
        entity_rc: &Rc<RefCell<Entity>>,
    ) -> bool {
        let entity_id = entity_rc.borrow().id;
        // The sentinel id marks the passing grade — the refusals ride
        // the same enum.
        const PASS: u32 = u32::MAX;
        let verdict = self.aoi_map.find(entity_id).map(|cache| {
            if cache.is_gone() {
                WitnessNote::RemoveNotInManualAoI { entity: entity_id }
            } else if !cache.is_manually_added() {
                WitnessNote::NotManualAoI { entity: entity_id }
            } else {
                WitnessNote::RemoveNotInManualAoI { entity: PASS }
            }
        });
        match verdict {
            None => {
                ctx.warnings
                    .push(WitnessNote::RemoveNotInManualAoI { entity: entity_id });
                false
            }
            Some(WitnessNote::RemoveNotInManualAoI { entity: PASS }) => {
                self.remove_from_aoi(ctx, entity_rc, true);
                true
            }
            Some(note) => {
                ctx.warnings.push(note);
                false
            }
        }
    }

    /// `Witness::updateManualAoI` (`witness.cpp:3110`): validate first,
    /// then drop the manual types that fell out of the list and add the
    /// listed ones.
    pub fn update_manual_aoi(
        &mut self,
        ctx: &mut WitnessContext<'_>,
        list: &[u32],
        ignore_non_manual: bool,
    ) -> bool {
        let mut listed = Vec::with_capacity(list.len());
        for &entity_id in list {
            let Some(entity) = ctx.entities.get(&entity_id) else {
                return false;
            };
            let face = ctx.type_face(&entity.borrow());
            if !face.manual_aoi {
                if ignore_non_manual {
                    ctx.warnings
                        .push(WitnessNote::NotManualAoIType { entity: entity_id });
                } else {
                    return false;
                }
            }
            listed.push(Rc::clone(entity));
        }
        // The visitor pass: caches no longer in the list leave it. The
        // type face decides — a non-manual hand add is never dropped
        // here.
        let stale: Vec<u32> = self
            .aoi_map
            .caches
            .values()
            .filter(|cache| !cache.is_gone() && !list.contains(&cache.entity))
            .map(|cache| cache.entity)
            .collect();
        for entity_id in stale {
            let Some(entity) = ctx.entities.get(&entity_id).cloned() else {
                continue;
            };
            if ctx.type_face(&entity.borrow()).manual_aoi {
                self.remove_from_aoi(ctx, &entity, true);
            }
        }
        for entity in listed {
            let face = ctx.type_face(&entity.borrow());
            if face.manual_aoi {
                self.add_to_aoi(ctx, &entity, true);
            }
        }
        true
    }

    /// `Witness::entitiesInManualAoI`: the manual adds still visible.
    pub fn entities_in_manual_aoi(&self) -> Vec<u32> {
        self.aoi_map
            .caches
            .values()
            .filter(|cache| cache.is_manually_added() && !cache.is_gone())
            .map(|cache| cache.entity)
            .collect()
    }

    // ----- The bandwidth faces ----------------------------------------

    /// `Witness::setAoIRadius` (`witness.cpp:2109`): floor at 1mm,
    /// remember the hysteresis, clamp to `maxAoIRadius`. The caller
    /// moves the trigger range and drops the mid-change crossings (the
    /// `callbacksPermitted( false )` quiet window).
    pub fn set_aoi_radius(&mut self, radius: f32, hyst: f32, max_radius: f32) {
        let radius = radius.max(0.1);
        self.aoi_hyst = hyst;
        self.aoi_radius = radius;
        if self.aoi_radius > max_radius {
            self.aoi_radius = max_radius;
        }
    }

    /// The clamp note the facade records once the max is known.
    pub fn radius_clamp_note(&self, asked: f32, max_radius: f32) -> Option<WitnessNote> {
        if asked > max_radius {
            Some(WitnessNote::RadiusClamped {
                id: self.entity_id,
                radius: asked,
                max: max_radius,
            })
        } else {
            None
        }
    }

    /// `Witness::setWitnessCapacity` (`witness.cpp:2006`) +
    /// `ServerUtil::bpsToPacketSize`.
    pub fn set_witness_capacity(&mut self, bps: i32, update_hertz: u32) {
        self.max_packet_size = bps / (8 * update_hertz as i32) - UDP_OVERHEAD;
    }

    /// `Witness::sendToClient` (`witness.cpp:1962`): select the entity
    /// the payload addresses and record the message. The bandwidth
    /// spend joins the deficit the next update pays back.
    pub fn send_to_client(
        &mut self,
        ctx: &mut WitnessContext<'_>,
        entity_id: u32,
        message_id: u16,
        size: i16,
    ) -> bool {
        let known_updatable = self
            .aoi_map
            .find(entity_id)
            .map(|cache| cache.is_updatable())
            .unwrap_or(false);
        if !known_updatable {
            ctx.errors.push(WitnessNote::SelectOutsideAoI {
                witness: self.entity_id,
                target: entity_id,
            });
            return false;
        }
        self.wire.push(Wire::SendMessageToClient {
            entity_id,
            message_id,
            size,
        });
        self.bandwidth_deficit += size as i32;
        true
    }

    // ----- The noise face ---------------------------------------------

    /// `Witness::newPosition` (`witness.cpp:2371`): sample the velocity
    /// once per tick past the first, and propagate the noise ten ticks
    /// after it was made.
    pub fn new_position(
        &mut self,
        now: u32,
        stealth_factor: f32,
        vertical_speed: f32,
        horizontal_speed_squared: f32,
        velocity: [f32; 3],
    ) {
        self.stealth_factor = stealth_factor;
        let dur = now.wrapping_sub(self.noise_check_time);
        let since_noise = now.wrapping_sub(self.noise_propagated_time);
        if dur > 1 && !self.noise_made {
            if velocity[1] < -vertical_speed {
                self.noise_made = true;
            } else {
                let horiz_speed_sqr = velocity[0] * velocity[0] + velocity[2] * velocity[2];
                if horiz_speed_sqr > horizontal_speed_squared {
                    self.noise_made = true;
                }
            }
            self.noise_check_time = now;
        }
        if since_noise > 10 && self.noise_made {
            self.noises.push((self.stealth_factor, 0));
            self.noise_made = false;
            self.noise_propagated_time = now;
        }
    }
}

/// The `Witness` setup refusals the `CellApp` facade can hit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WitnessSetup {
    /// The entity is not in the population.
    UnknownEntity,
    /// The creation stream did not carry the witness byte.
    NoWitness,
    /// The entity is not on a range list yet.
    NoRangeNode,
}

impl CellApp {
    /// The per-call context the witness faces borrow: every field a
    /// `Witness` method needs, split out of `self`.
    pub fn witness_context(&mut self) -> WitnessContext<'_> {
        WitnessContext {
            entities: &self.entity_population,
            type_faces: &self.entity_type_faces,
            properties: &self.entity_properties,
            schemes: &self.aoi_schemes,
            scripts: &mut self.script_calls,
            warnings: &mut self.witness_warnings,
            errors: &mut self.witness_errors,
            throttle: self.emergency_throttle,
            max_priority_delta: self.witness_max_priority_delta,
            update_bytes: self.witness_update_bytes,
            spam_size: self.witness_spam_size,
            delta_growth_throttle: 1.0 + 1.0 / 8.0,
        }
    }

    /// The `Witness` ctor + `AoITrigger` wiring (`witness.cpp:124`'s
    /// `init`): the trigger wants both AoI crossings and makes none,
    /// centred on the witnessing entity's range node.
    pub fn add_witness(&mut self, space_id: SpaceId, entity_id: u32) -> Result<(), WitnessSetup> {
        let entity_rc = self
            .entity_population
            .get(&entity_id)
            .cloned()
            .ok_or(WitnessSetup::UnknownEntity)?;
        if !entity_rc.borrow().has_witness {
            return Err(WitnessSetup::NoWitness);
        }
        let node = entity_rc
            .borrow()
            .range_node
            .ok_or(WitnessSetup::NoRangeNode)?;
        let radius = self.max_aoi_radius;
        let mut witness = Witness::new(entity_id, space_id, radius);
        let events = Rc::clone(&witness.aoi_events);
        let list = self.range_lists.entry(space_id).or_default();
        let trigger = list.create_trigger(
            node,
            radius,
            FLAG_LOWER_AOI_TRIGGER | FLAG_UPPER_AOI_TRIGGER,
            FLAG_LOWER_AOI_TRIGGER | FLAG_UPPER_AOI_TRIGGER,
            FLAG_NO_TRIGGERS,
            FLAG_NO_TRIGGERS,
            TriggerTarget::AoI(events),
        );
        list.trigger_insert(trigger);
        list.entity_add_trigger(&entity_rc, trigger);
        witness.aoi_trigger = Some(trigger);
        self.witnesses.insert(entity_id, witness);
        Ok(())
    }

    /// The deferred `addToAoI`/`removeFromAoI` crossings the AoI
    /// trigger recorded, applied in order.
    pub fn pump_aoi_events(&mut self, witness_id: u32) {
        let Some(events) = self
            .witnesses
            .get(&witness_id)
            .map(|witness| witness.take_aoi_events())
        else {
            return;
        };
        let mut ctx = WitnessContext {
            entities: &self.entity_population,
            type_faces: &self.entity_type_faces,
            properties: &self.entity_properties,
            schemes: &self.aoi_schemes,
            scripts: &mut self.script_calls,
            warnings: &mut self.witness_warnings,
            errors: &mut self.witness_errors,
            throttle: self.emergency_throttle,
            max_priority_delta: self.witness_max_priority_delta,
            update_bytes: self.witness_update_bytes,
            spam_size: self.witness_spam_size,
            delta_growth_throttle: 1.0 + 1.0 / 8.0,
        };
        let witness = self
            .witnesses
            .get_mut(&witness_id)
            .expect("the witness was just read");
        for (entity_id, entering) in events {
            let Some(entity_rc) = ctx.entities.get(&entity_id).cloned() else {
                continue;
            };
            if entering {
                witness.add_to_aoi(&mut ctx, &entity_rc, false);
            } else {
                witness.remove_from_aoi(&mut ctx, &entity_rc, false);
            }
        }
    }

    /// `Witness::update` through the context.
    pub fn process_witness(&mut self, witness_id: u32) {
        if !self.witnesses.contains_key(&witness_id) {
            return;
        }
        let mut ctx = WitnessContext {
            entities: &self.entity_population,
            type_faces: &self.entity_type_faces,
            properties: &self.entity_properties,
            schemes: &self.aoi_schemes,
            scripts: &mut self.script_calls,
            warnings: &mut self.witness_warnings,
            errors: &mut self.witness_errors,
            throttle: self.emergency_throttle,
            max_priority_delta: self.witness_max_priority_delta,
            update_bytes: self.witness_update_bytes,
            spam_size: self.witness_spam_size,
            delta_growth_throttle: 1.0 + 1.0 / 8.0,
        };
        let witness = self
            .witnesses
            .get_mut(&witness_id)
            .expect("the witness was just checked");
        witness.update(&mut ctx);
    }

    /// `Witness::requestEntityUpdate` through the context.
    pub fn request_entity_update(&mut self, witness_id: u32, target: u32, stamps: &[EventNumber]) {
        if !self.witnesses.contains_key(&witness_id) {
            return;
        }
        let mut ctx = WitnessContext {
            entities: &self.entity_population,
            type_faces: &self.entity_type_faces,
            properties: &self.entity_properties,
            schemes: &self.aoi_schemes,
            scripts: &mut self.script_calls,
            warnings: &mut self.witness_warnings,
            errors: &mut self.witness_errors,
            throttle: self.emergency_throttle,
            max_priority_delta: self.witness_max_priority_delta,
            update_bytes: self.witness_update_bytes,
            spam_size: self.witness_spam_size,
            delta_growth_throttle: 1.0 + 1.0 / 8.0,
        };
        let witness = self
            .witnesses
            .get_mut(&witness_id)
            .expect("the witness was just checked");
        witness.request_entity_update(&mut ctx, target, stamps);
    }

    /// `~Witness` (`witness.cpp:605`): the entity forgets the trigger,
    /// the trigger leaves the range list without contracting — the
    /// offload face owns the leaves.
    pub fn remove_witness(&mut self, entity_id: u32) -> bool {
        let Some(witness) = self.witnesses.remove(&entity_id) else {
            return false;
        };
        let Some(trigger) = witness.aoi_trigger else {
            return true;
        };
        let Some(entity_rc) = self.entity_population.get(&entity_id).cloned() else {
            return true;
        };
        if let Some(list) = self.range_lists.get_mut(&witness.space_id) {
            // The entity teardown (`RangeList::delEntity`) may already
            // have carried the trigger off; `~Witness`'s `delTrigger`
            // then only forgets the pointer.
            if list.trigger(trigger).is_some() {
                list.entity_del_trigger(&entity_rc, trigger);
                list.trigger_remove_without_contracting(trigger);
            }
        }
        true
    }

    /// `Witness::setAoIRadius`'s full face: the witness state, the
    /// trigger range move, and the mid-change crossings dropped (the
    /// `callbacksPermitted( false )` quiet window).
    pub fn set_witness_aoi_radius(&mut self, entity_id: u32, radius: f32, hyst: f32) -> bool {
        let max_radius = self.max_aoi_radius;
        let (radius, space_id, trigger, events, note) = {
            let Some(witness) = self.witnesses.get_mut(&entity_id) else {
                return false;
            };
            let note = witness.radius_clamp_note(radius, max_radius);
            witness.set_aoi_radius(radius, hyst, max_radius);
            (
                witness.aoi_radius,
                witness.space_id,
                witness.aoi_trigger,
                Rc::clone(&witness.aoi_events),
                note,
            )
        };
        if let Some(note) = note {
            self.witness_warnings.push(note);
        }
        if let (Some(trigger), Some(list)) = (trigger, self.range_lists.get_mut(&space_id)) {
            list.trigger_set_range(trigger, radius);
        }
        events.borrow_mut().clear();
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cellapp_real_entity::{ADD_ENTITY_DATA_IS_ON_GROUND, EntityScript};
    use crate::cellapp_space::Space;

    const SID: SpaceId = 5;
    const TYPE: u32 = 9;

    /// The creation stream: id + type, position, flags, direction, last
    /// event number 42, base address, witness byte.
    fn stream_for(id: u32, type_id: u32, pos: [f32; 3], witness: u8) -> Vec<u8> {
        let mut stream = Vec::new();
        stream.extend_from_slice(&id.to_le_bytes());
        stream.extend_from_slice(&type_id.to_le_bytes());
        for value in pos {
            stream.extend_from_slice(&value.to_le_bytes());
        }
        stream.push(ADD_ENTITY_DATA_IS_ON_GROUND);
        for value in [0.25f32, 0.0, 0.0] {
            stream.extend_from_slice(&value.to_le_bytes());
        }
        stream.extend_from_slice(&42u32.to_le_bytes());
        stream.extend_from_slice(&7u32.to_le_bytes());
        stream.extend_from_slice(&6502u16.to_le_bytes());
        stream.push(witness);
        stream
    }

    fn ctx_of(app: &mut CellApp) -> WitnessContext<'_> {
        WitnessContext {
            entities: &app.entity_population,
            type_faces: &app.entity_type_faces,
            properties: &app.entity_properties,
            schemes: &app.aoi_schemes,
            scripts: &mut app.script_calls,
            warnings: &mut app.witness_warnings,
            errors: &mut app.witness_errors,
            throttle: app.emergency_throttle,
            max_priority_delta: app.witness_max_priority_delta,
            update_bytes: app.witness_update_bytes,
            spam_size: app.witness_spam_size,
            delta_growth_throttle: 1.0 + 1.0 / 8.0,
        }
    }

    /// Splits the borrows: the context over its fields, the witness over
    /// the map slot.
    fn ctx_with<'a>(app: &'a mut CellApp, id: u32) -> (WitnessContext<'a>, &'a mut Witness) {
        let ctx = WitnessContext {
            entities: &app.entity_population,
            type_faces: &app.entity_type_faces,
            properties: &app.entity_properties,
            schemes: &app.aoi_schemes,
            scripts: &mut app.script_calls,
            warnings: &mut app.witness_warnings,
            errors: &mut app.witness_errors,
            throttle: app.emergency_throttle,
            max_priority_delta: app.witness_max_priority_delta,
            update_bytes: app.witness_update_bytes,
            spam_size: app.witness_spam_size,
            delta_growth_throttle: 1.0 + 1.0 / 8.0,
        };
        (ctx, app.witnesses.get_mut(&id).expect("the witness"))
    }

    /// Witness 1 at the origin with the fixture type, target 2 parked
    /// outside the 500 radius, and one client property on the target
    /// stamped above its LoD floor.
    fn fixture(witness_byte: u8) -> (CellApp, BTreeMap<SpaceId, Space>) {
        let mut app = CellApp::default();
        let mut spaces = BTreeMap::new();
        spaces.insert(SID, Space::new(SID));
        app.entity_scripts
            .insert(TYPE, EntityScript::with_init(vec![]));
        app.create_real_entity(
            &mut spaces,
            SID,
            &stream_for(1, TYPE, [0.0; 3], witness_byte),
        );
        app.create_real_entity(
            &mut spaces,
            SID,
            &stream_for(2, TYPE, [3000.0, 0.0, 0.0], b'-'),
        );
        app.entity_type_faces.insert(TYPE, EntityTypeDef::default());
        for witness in app.witnesses.values_mut() {
            witness.set_witness_capacity(500_000, 10);
        }
        app.entity_properties.insert(
            2,
            vec![ClientProperty {
                level: 0,
                stamp: 100,
            }],
        );
        (app, spaces)
    }

    /// A range-list shuffle to a new position, with no own triggers.
    fn move_entity(app: &mut CellApp, id: u32, pos: [f32; 3]) {
        let entity = app.entity_population[&id].clone();
        let node = entity.borrow().range_node.expect("a ranged entity");
        let old = entity.borrow().position;
        entity.borrow_mut().position = pos;
        app.range_lists
            .get_mut(&SID)
            .expect("the space list")
            .shuffle_entity(node, &[], old[0], old[2]);
    }

    /// Walks the target into the AoI and runs one full observation:
    /// pump the crossings, drain the pending machines, ack the enter,
    /// drain again — the wire tail this emits.
    fn observe_once(app: &mut CellApp) -> Vec<Wire> {
        move_entity(app, 2, [3.0, 0.0, 4.0]);
        app.pump_aoi_events(1);
        app.process_witness(1);
        app.request_entity_update(1, 2, &[5]);
        app.process_witness(1);
        let start = app.witnesses[&1].wire.len() - 2;
        app.witnesses[&1].wire[start..].to_vec()
    }

    fn enter_of(wire: &[Wire]) -> (u32, u8) {
        match &wire[0] {
            Wire::EnterAoI { id, id_alias } => (*id, *id_alias),
            other => panic!("expected an enter, got {other:?}"),
        }
    }

    fn create_of(wire: &[Wire]) -> (u32, bool, DetailLevel, Vec<u8>) {
        match &wire[1] {
            Wire::CreateEntity {
                id,
                detailed,
                detail_level,
                property_indices,
            } => (*id, *detailed, *detail_level, property_indices.clone()),
            other => panic!("expected a create, got {other:?}"),
        }
    }

    #[test]
    #[should_panic(expected = "expected an enter")]
    fn enter_of_refuses_the_wrong_face() {
        enter_of(&[Wire::TickSync { tick_byte: 0 }]);
    }

    #[test]
    #[should_panic(expected = "expected a create")]
    fn create_of_refuses_the_wrong_face() {
        create_of(&[
            Wire::TickSync { tick_byte: 0 },
            Wire::TickSync { tick_byte: 0 },
        ]);
    }

    // ----- The acceptance faces ---------------------------------------

    #[test]
    fn the_first_observation_runs_enter_then_create() {
        let (mut app, _spaces) = fixture(b'W');
        move_entity(&mut app, 2, [3.0, 0.0, 4.0]);
        app.pump_aoi_events(1);
        let witness = app.witnesses.get(&1).expect("the wired witness");
        let cache = witness.aoi_map.find(2).expect("the observed cache");
        assert!(cache.is_enter_pending());
        assert_eq!(witness.aoi_map.num_in_aoi, 1);
        assert!(
            app.script_calls
                .iter()
                .any(|call| call.entity == 1 && call.name == "onEnteredAoI")
        );

        app.process_witness(1);
        app.request_entity_update(1, 2, &[5]);
        app.process_witness(1);
        let wire = &app.witnesses[&1].wire;
        assert!(matches!(wire[0], Wire::TickSync { tick_byte: 0 }));
        assert_eq!(enter_of(&wire[1..]), (2, 254));
        assert_eq!(create_of(&wire[1..]), (2, false, 0, vec![0]));
    }

    /// BW-08-03-02's acceptance: the second observation replays the
    /// first one's wire byte for byte.
    #[test]
    fn the_repeated_entry_matches_the_first() {
        let (mut app, _spaces) = fixture(b'W');
        let first = observe_once(&mut app);

        move_entity(&mut app, 2, [3000.0, 0.0, 0.0]);
        app.pump_aoi_events(1);
        app.process_witness(1);
        let second = observe_once(&mut app);

        assert_eq!(first, second);
    }

    // ----- The alias pool ---------------------------------------------

    #[test]
    fn the_alias_pool_excludes_the_reserved_alias_and_reuses() {
        let (mut app, _spaces) = fixture(b'W');
        {
            let witness = app.witnesses.get(&1).expect("the witness");
            assert_eq!(witness.num_free_aliases, 255);
            assert_eq!(witness.free_aliases[0], 0);
            assert_eq!(witness.free_aliases[254], 254);
            assert!(!witness.free_aliases[..255].contains(&NO_ID_ALIAS));
        }

        move_entity(&mut app, 2, [3.0, 0.0, 4.0]);
        app.pump_aoi_events(1);
        app.process_witness(1);
        assert_eq!(enter_of(&app.witnesses[&1].wire[1..]), (2, 254));
        assert_eq!(app.witnesses[&1].num_free_aliases, 254);

        // Finish the observation so the cache is updatable; an
        // enter-pending cache that goes gone keeps its alias until the
        // client acks.
        app.request_entity_update(1, 2, &[5]);
        app.process_witness(1);

        move_entity(&mut app, 2, [3000.0, 0.0, 0.0]);
        app.pump_aoi_events(1);
        app.process_witness(1);
        assert_eq!(app.witnesses[&1].num_free_aliases, 255);

        move_entity(&mut app, 2, [3.0, 0.0, 4.0]);
        app.pump_aoi_events(1);
        app.process_witness(1);
        assert_eq!(enter_of(&app.witnesses[&1].wire[4..]), (2, 254));
    }

    #[test]
    fn the_alias_faces_skip_volatile_less_and_drained_pools() {
        let (mut app, mut spaces) = fixture(b'W');
        app.entity_scripts
            .insert(10, EntityScript::with_init(vec![]));
        app.create_real_entity(&mut spaces, SID, &stream_for(3, 10, [1.0, 0.0, 1.0], b'-'));
        app.entity_type_faces.insert(
            10,
            EntityTypeDef {
                has_volatile: false,
                ..EntityTypeDef::default()
            },
        );
        {
            let entity = app.entity_population[&3].clone();
            let (mut ctx, witness) = ctx_with(&mut app, 1);
            assert!(witness.add_to_manual_aoi(&mut ctx, &entity));
        }
        app.process_witness(1);
        assert_eq!(enter_of(&app.witnesses[&1].wire[1..]), (3, NO_ID_ALIAS));
        assert_eq!(app.witnesses[&1].num_free_aliases, 255);

        move_entity(&mut app, 3, [3000.0, 0.0, 0.0]);
        app.pump_aoi_events(1);
        app.process_witness(1);
        assert_eq!(app.witnesses[&1].num_free_aliases, 255);

        // A drained pool hands out the full-id alias too.
        let mut cache = EntityCache::new(4, 0, 1, 0, 0);
        cache.set_enter_pending();
        app.witnesses
            .get_mut(&1)
            .expect("the witness")
            .aoi_map
            .add(cache);
        let mut witness = app.witnesses.remove(&1).expect("the witness");
        let ctx = ctx_of(&mut app);
        witness.num_free_aliases = 0;
        let alias = witness.allocate_id_alias(&ctx, &Entity::new(4, true));
        assert_eq!(alias, NO_ID_ALIAS);

        // A leave for an unknown cache is silent.
        witness.on_leave_aoi(999);
    }

    // ----- The crossing faces -----------------------------------------

    #[test]
    fn the_crossings_that_name_nobody_are_dropped() {
        let (mut app, mut spaces) = fixture(b'W');
        app.entity_scripts
            .insert(10, EntityScript::with_init(vec![]));
        // A non-client type never caches and its removal never blames
        // the map.
        app.create_real_entity(&mut spaces, SID, &stream_for(3, 10, [1.0, 0.0, 1.0], b'-'));
        app.entity_type_faces.insert(
            10,
            EntityTypeDef {
                can_be_on_client: false,
                ..EntityTypeDef::default()
            },
        );
        {
            let entity = app.entity_population[&3].clone();
            let (mut ctx, witness) = ctx_with(&mut app, 1);
            witness.add_to_aoi(&mut ctx, &entity, false);
            witness.remove_from_aoi(&mut ctx, &entity, false);
            assert!(witness.aoi_map.find(3).is_none());
        }
        assert!(app.witness_errors.is_empty());

        app.witnesses[&1].aoi_events.borrow_mut().push((999, true));
        app.pump_aoi_events(1);
        assert!(app.witnesses[&1].aoi_map.find(999).is_none());
    }

    #[test]
    fn the_witness_never_caches_itself() {
        let (mut app, _spaces) = fixture(b'W');
        move_entity(&mut app, 2, [3.0, 0.0, 4.0]);
        app.pump_aoi_events(1);
        assert_eq!(app.witnesses[&1].entities_in_aoi(), vec![2]);
        assert!(app.witnesses[&1].aoi_map.find(1).is_none());
    }

    #[test]
    fn the_destroyed_pair_add_and_remove_ignore_each_other() {
        let (mut app, _spaces) = fixture(b'W');
        app.entity_population[&2].borrow_mut().destroyed = true;
        {
            let entity = app.entity_population[&2].clone();
            let (mut ctx, witness) = ctx_with(&mut app, 1);
            witness.add_to_aoi(&mut ctx, &entity, false);
            assert!(witness.in_aoi_offload);
            witness.remove_from_aoi(&mut ctx, &entity, false);
            assert!(!witness.in_aoi_offload);
            assert!(witness.aoi_map.find(2).is_none());
        }
        // The cleared pair leaves the normal faces working.
        app.entity_population[&2].borrow_mut().destroyed = false;
        move_entity(&mut app, 2, [3.0, 0.0, 4.0]);
        app.pump_aoi_events(1);
        assert!(app.witnesses[&1].aoi_map.find(2).is_some());
    }

    // ----- The cache map and the cache state --------------------------

    #[test]
    fn the_cache_map_counts_are_exact() {
        let mut map = EntityCacheMap::default();
        map.add(EntityCache::new(1, 0, 1, 0, 0));
        map.add(EntityCache::new(2, 0, 1, 0, 0));
        assert_eq!(map.size(), 2);
        assert_eq!(map.num_in_aoi, 2);
        assert_eq!(map.num_in_aoi_ever, 2);
        map.del(1);
        map.del(1);
        assert_eq!(map.size(), 1);
        assert_eq!(map.num_in_aoi, 1);
        assert_eq!(map.num_in_aoi_ever, 2);
        assert!(map.find(2).is_some());
        assert!(map.find_mut(1).is_none());
    }

    #[test]
    fn the_client_state_resets_with_the_server_state_kept() {
        let mut cache = EntityCache::new(1, 3, 2, 7, 0);
        assert_eq!(cache.last_event_number, 7);
        assert_eq!(cache.last_volatile_update_number, 65535);
        assert_eq!(cache.detail_level, 2);
        assert_eq!(cache.id_alias, NO_ID_ALIAS);
        assert_eq!(cache.last_priority_delta, f64::MAX);
        assert_eq!(cache.update_scheme_id, 3);

        cache.set_withheld();
        cache.set_manually_added();
        cache.set_enter_pending();
        cache.set_request_pending();
        cache.on_entity_removed_from_client(9, 4);
        assert!(cache.is_withheld());
        assert!(cache.is_manually_added());
        assert!(!cache.is_enter_pending());
        assert!(!cache.is_request_pending());
        assert_eq!(cache.last_event_number, 9);
        assert_eq!(cache.last_volatile_update_number, 3);
        assert!(!cache.is_always_detailed());
        cache.set_always_detailed(true);
        assert!(cache.is_always_detailed());

        // The prioritised flag rides the same word.
        cache.set_prioritised();
        assert!(cache.is_prioritised());
        cache.clear_prioritised();
        assert!(!cache.is_prioritised());
    }

    // ----- The priority faces -----------------------------------------

    #[test]
    fn the_priority_growth_is_throttled_by_the_last_delta() {
        let schemes = AoIUpdateSchemes::default();
        let mut cache = EntityCache::new(1, 0, 1, 0, 0);
        let origin = [0.0; 3];
        let near = Entity::new(1, true);
        cache.update_priority(&schemes, 1.125, origin, &near);
        // apply(0) = 1, uncapped on the DBL_MAX denominator.
        assert!((cache.priority - 1.0).abs() < 1e-9);
        let mut far = Entity::new(1, true);
        far.position = [30.0, 0.0, 40.0];
        cache.update_priority(&schemes, 1.125, origin, &far);
        // apply(50m) = 11, capped at 1.125 * 1.
        assert!((cache.priority - 2.125).abs() < 1e-9);
        cache.set_priority(1.5);
        assert!((cache.priority - 1.5).abs() < 1e-9);
    }

    #[test]
    fn the_distance_squared_clamps_at_float_max() {
        let cache = EntityCache::new(1, 0, 1, 0, 0);
        let mut far = Entity::new(1, true);
        far.position = [2.0e19, 0.0, 2.0e19];
        assert_eq!(cache.distance_squared([0.0; 3], &far), f32::MAX);
        let near = Entity::new(1, true);
        assert_eq!(cache.distance_squared([3.0, 0.0, 4.0], &near), 25.0);
    }

    #[test]
    fn the_coincident_scheme_pins_the_priority_to_zero() {
        let mut schemes = AoIUpdateSchemes::default();
        let id = schemes
            .register("coincident", 0.0, 0.0, 500.0)
            .expect("the zero pair is legal");
        let mut cache = EntityCache::new(1, id, 1, 0, 0);
        let mut entity = Entity::new(1, true);
        entity.position = [30.0, 0.0, 40.0];
        assert!(schemes.should_treat_as_coincident(id));
        assert_eq!(schemes.apply(id, 2500.0), 1.0);
        assert_eq!(cache.lod_priority(&schemes, [0.0; 3], &entity), 0.0);
        cache.update_priority(&schemes, 1.125, [0.0; 3], &entity);
        assert!((cache.priority - 1.0).abs() < 1e-9);
        assert!(!schemes.should_treat_as_coincident(0));
        // The same cache pointed at the default scheme reports the raw
        // distance again.
        cache.update_scheme_id = 0;
        assert_eq!(cache.lod_priority(&schemes, [0.0; 3], &entity), 2500.0);
    }

    // ----- The scheme table -------------------------------------------

    #[test]
    fn the_scheme_table_resolves_names_and_ids() {
        let mut schemes = AoIUpdateSchemes::with_default(1.0, 101.0, 500.0);
        assert_eq!(schemes.get_id_from_name(""), Some(0));
        assert_eq!(schemes.get_id_from_name("default"), Some(0));
        assert_eq!(schemes.get_id_from_name("scout"), None);
        assert_eq!(schemes.get_name_from_id(0), Some("default"));
        assert_eq!(schemes.get_name_from_id(9), None);
        // The default scheme at the 500 radius: (500 * 0.2 + 1) * 1.
        assert!((schemes.apply(0, 250000.0) - 101.0).abs() < 1e-6);

        let scout = schemes
            .register("scout", 2.0, 10.0, 500.0)
            .expect("a fresh name registers");
        assert_eq!(scout, 1);
        assert_eq!(schemes.get_id_from_name("scout"), Some(1));
        // (4 * 0.008 + 1) * 2.
        assert!((schemes.apply(1, 16.0) - 2.064).abs() < 1e-6);
    }

    #[test]
    fn the_scheme_registration_rejects_bad_names_and_wraparound() {
        let mut schemes = AoIUpdateSchemes::default();
        assert!(schemes.register("", 1.0, 2.0, 500.0).is_err());
        assert!(schemes.register("default", 1.0, 2.0, 500.0).is_err());
        for index in 0..255 {
            let name = format!("scheme{index}");
            schemes
                .register(&name, 1.0, 2.0, 500.0)
                .expect("the scheme registers");
        }
        assert_eq!(
            schemes.register("one-too-many", 1.0, 2.0, 500.0),
            Err("too many schemes defined")
        );
    }

    #[test]
    fn the_scheme_init_validates_the_delta_pair() {
        let mut scheme = AoiUpdateScheme::default();
        assert_eq!(
            scheme.init(-1.0, 1.0, 500.0),
            Err("deltas must be non-negative")
        );
        assert_eq!(
            scheme.init(0.0, 1.0, 500.0),
            Err("both deltas must be zero or both positive")
        );
        assert_eq!(
            scheme.init(1.0, -1.0, 500.0),
            Err("deltas must be non-negative")
        );
        scheme.init(0.0, 0.0, 500.0).expect("the zero pair");
        assert!(scheme.should_treat_as_coincident());
        scheme.init(1.0, 101.0, 500.0).expect("the default pair");
        assert!((scheme.weighting - 1.0).abs() < 1e-9);
        assert!((scheme.distance_weighting - 0.2).abs() < 1e-6);
    }

    // ----- The LoD faces ----------------------------------------------

    #[test]
    fn the_lod_windows_square_the_starts() {
        let lods = LodLevels::from_starts(&[(50.0, 5.0), (10.0, 2.0)]);
        assert_eq!(lods.size(), 2);
        assert!(lods.levels[0].low == -1.0);
        assert!((lods.levels[0].high - 144.0).abs() < 1e-4);
        assert!((lods.levels[1].low - 100.0).abs() < 1e-4);
        assert_eq!(lods.levels[1].high, f32::MAX);
        assert!(lods.needs_more_detail(0, -2.0));
        assert!(lods.needs_less_detail(0, 200.0));
        assert!(!lods.needs_less_detail(1, f32::MAX));

        let single = LodLevels::single();
        assert_eq!(single.size(), 1);
        assert!(single.needs_more_detail(1, 1000.0));
        assert!(!single.needs_less_detail(1, 1000.0));
    }

    #[test]
    fn the_lod_sentinel_descends_once() {
        let mut app = CellApp::default();
        let ctx = app.witness_context();
        let lods = LodLevels::single();
        let schemes = AoIUpdateSchemes::default();
        let mut cache = EntityCache::new(1, 0, 1, 0, 0);
        let entity = Entity::new(1, true);
        let mut wire = Vec::new();
        let wrote = cache.update_detail_level(
            &ctx,
            &mut wire,
            &lods,
            cache.lod_priority(&schemes, [0.0; 3], &entity),
            false,
        );
        assert!(!wrote);
        assert_eq!(cache.detail_level, 0);
    }

    #[test]
    fn the_lod_stamp_seed_walks_the_update_detail() {
        let lods = LodLevels::from_starts(&[(10.0, 0.0)]);
        assert_eq!(lods.size(), 1);
        let mut app = CellApp::default();
        app.entity_properties
            .insert(1, vec![ClientProperty { level: 0, stamp: 3 }]);
        let ctx = ctx_of(&mut app);
        let mut cache = EntityCache::new(1, 0, 1, 2, 0);
        let mut wire = Vec::new();
        // The sentinel's FLT_MAX low walks any finite priority down a
        // level, and the pending property flushes with the select
        // header.
        let wrote = cache.update_detail_level(&ctx, &mut wire, &lods, 200.0, false);
        assert!(wrote);
        assert_eq!(cache.detail_level, 0);
        assert_eq!(wire.len(), 2);
        assert!(matches!(&wire[0], Wire::SelectEntity { id: 1 }));
        assert!(matches!(
            &wire[1],
            Wire::UpdateEntity { id: 1, num: 1, level: 0, indices, selected: true }
                if indices == &vec![0]
        ));

        // At level 0 the walk stops: -1 is the floor and FLT_MAX the
        // ceiling of the only real level.
        let mut wire = Vec::new();
        let wrote = cache.update_detail_level(&ctx, &mut wire, &lods, 4.0, true);
        assert!(!wrote);
        assert!(wire.is_empty());

        // A two-level cache walking down past a selected entity leaves
        // the select out but still starts the message — the sentinel
        // row is the level past the table.
        let mut other = EntityCache::new(1, 0, 2, 2, 0);
        other.set_lod_event_number(0, 1);
        let two = LodLevels::from_starts(&[(10.0, 0.0), (30.0, 0.0)]);
        assert_eq!(two.size(), 2);
        let mut wire = Vec::new();
        let wrote = other.update_detail_level(&ctx, &mut wire, &two, 4.0, true);
        assert!(wrote);
        assert_eq!(other.detail_level, 0);
        assert_eq!(wire.len(), 1);
        assert!(matches!(
            &wire[0],
            Wire::UpdateEntity { id: 1, num: 1, level: 0, indices, selected: false }
                if indices == &vec![0]
        ));

        // Without the header the update rides bare — the client
        // already knows the entity.
        let mut bare = EntityCache::new(1, 0, 1, 0, 0);
        bare.detail_level = 0;
        let mut wire = Vec::new();
        assert!(bare.add_changed_properties(&ctx, &mut wire, false, true));
        assert_eq!(wire.len(), 1);
        assert!(matches!(
            &wire[0],
            Wire::UpdateEntity { id: 1, num: 1, level: 0, indices, selected: false }
                if indices == &vec![0]
        ));
    }

    #[test]
    fn the_lod_walk_stamps_the_levels_it_leaves() {
        let mut app = CellApp::default();
        let ctx = app.witness_context();
        let lods = LodLevels::from_starts(&[(10.0, 0.0), (30.0, 5.0)]);
        assert_eq!(lods.size(), 2);
        let mut cache = EntityCache::new(1, 0, 2, 7, 0);
        cache.detail_level = 0;
        let mut wire = Vec::new();
        // Above the outer window: the walk leaves level 0 and stamps
        // it with the event number — nothing goes on the wire.
        let wrote = cache.update_detail_level(&ctx, &mut wire, &lods, 200.0, false);
        assert!(!wrote);
        assert!(wire.is_empty());
        assert_eq!(cache.detail_level, 1);
        assert_eq!(cache.lod_event_number(0), 7);
    }

    // ----- The wire messages ------------------------------------------

    #[test]
    fn the_leave_message_stamps_stored_and_current() {
        let mut cache = EntityCache::new(9, 0, 2, 7, 3);
        cache.detail_level = 1;
        cache.set_lod_event_number(0, 9);
        let mut wire = Vec::new();
        cache.add_leave_aoi_message(&mut wire, 9);
        assert!(matches!(&wire[0], Wire::LeaveAoI { id: 9, .. }));
        assert!(matches!(
            &wire[0],
            Wire::LeaveAoI { lod_stamps, .. } if lod_stamps == &vec![9, 7]
        ));
        wire.clear();

        // The seed takes the top levels in order.
        cache.set_lod_event_numbers(&[1, 2]);
        assert_eq!(cache.lod_event_number(0), 1);
        assert_eq!(cache.lod_event_number(1), 2);
    }

    #[test]
    fn the_select_message_names_by_alias_then_by_id() {
        let mut aliased = EntityCache::new(9, 0, 1, 0, 0);
        aliased.id_alias = 4;
        let mut wire = Vec::new();
        aliased.add_entity_select_message(&mut wire);
        assert!(matches!(
            wire.pop(),
            Some(Wire::SelectAliasedEntity { id_alias: 4 })
        ));

        let plain = EntityCache::new(9, 0, 1, 0, 0);
        let mut wire = Vec::new();
        plain.add_entity_select_message(&mut wire);
        assert!(matches!(wire.pop(), Some(Wire::SelectEntity { id: 9 })));
    }

    #[test]
    fn the_cache_stream_records_the_vehicle_state() {
        let mut entity = Entity::new(9, true);
        entity.vehicle_change_num = 3;
        entity.vehicle = Some(12);

        // Stale: the OLD state, no vehicle id.
        let stale = EntityCache::new(9, 0, 2, 7, 3);
        let bytes = stale.write_to_stream(&entity);
        assert_eq!(&bytes[0..4], &9u32.to_le_bytes());
        assert_eq!(&bytes[4..6], &stale.flags.to_le_bytes());
        assert_eq!(bytes[6], 0);
        assert_eq!(bytes[7], VEHICLE_CHANGE_NUM_OLD);
        assert_eq!(&bytes[8..16], &stale.priority.to_le_bytes());
        assert_eq!(&bytes[16..20], &7u32.to_le_bytes());
        assert_eq!(bytes[20], NO_ID_ALIAS);
        assert_eq!(bytes[21], 2);
        assert_eq!(bytes[22], 2);
        // The size - 1 stamps: one u32 for a two-level cache.
        assert_eq!(&bytes[23..27], &stale.lod_event_numbers[0].to_le_bytes());
        assert_eq!(bytes.len(), 27);

        // Agreed with a mount: the state byte then the vehicle id.
        let mut agreed = EntityCache::new(9, 0, 2, 7, 3);
        agreed.vehicle_change_num = 3;
        let bytes = agreed.write_to_stream(&entity);
        assert_eq!(bytes[7], VEHICLE_CHANGE_NUM_HAS_VEHICLE);
        assert_eq!(&bytes[27..31], &12u32.to_le_bytes());
        assert_eq!(bytes.len(), 31);

        // Agreed on foot.
        entity.vehicle = None;
        let bytes = agreed.write_to_stream(&entity);
        assert_eq!(bytes[7], VEHICLE_CHANGE_NUM_HAS_NO_VEHICLE);
        assert_eq!(bytes.len(), 27);
    }

    // ----- The reuse and removal machines -----------------------------

    #[test]
    fn the_reuse_revives_without_a_refresh_when_history_holds() {
        let (mut app, _spaces) = fixture(b'W');
        move_entity(&mut app, 2, [3.0, 0.0, 4.0]);
        app.pump_aoi_events(1);
        app.process_witness(1);
        move_entity(&mut app, 2, [3000.0, 0.0, 0.0]);
        app.pump_aoi_events(1);
        move_entity(&mut app, 2, [3.0, 0.0, 4.0]);
        app.pump_aoi_events(1);
        {
            let witness = app.witnesses.get(&1).expect("the witness");
            let cache = witness.aoi_map.find(2).expect("the revived cache");
            assert!(!cache.is_gone());
            assert!(!cache.is_refresh());
            assert!(cache.is_request_pending());
        }
        assert_eq!(app.witness_warnings.len(), 0);
    }

    #[test]
    fn the_reuse_marks_a_refresh_when_history_was_trimmed() {
        let (mut app, _spaces) = fixture(b'W');
        // The refresh face needs an updatable cache — a request-pending
        // one (the enter unacked) revives silently instead.
        let _ = observe_once(&mut app);
        move_entity(&mut app, 2, [3000.0, 0.0, 0.0]);
        app.pump_aoi_events(1);
        app.entity_population[&2]
            .borrow_mut()
            .last_trimmed_event_number = 50;
        move_entity(&mut app, 2, [3.0, 0.0, 4.0]);
        app.pump_aoi_events(1);
        {
            let witness = app.witnesses.get(&1).expect("the witness");
            let cache = witness.aoi_map.find(2).expect("the refreshed cache");
            assert!(cache.is_refresh());
            assert!(!cache.is_gone());
        }
        assert!(matches!(
            app.witness_warnings.pop(),
            Some(WitnessNote::ReuseMissedEvents {
                witness: 2,
                last_received: 42,
                entity_at: 42,
                history_since: 50,
            })
        ));

        // The refresh pass deletes and re-enters.
        app.process_witness(1);
        let wire = &app.witnesses[&1].wire;
        assert!(matches!(&wire[3], Wire::LeaveAoI { id: 2, .. }));
        assert!(matches!(&wire[4], Wire::EnterAoI { id: 2, .. }));
    }

    #[test]
    fn the_create_pass_washes_a_stray_refresh() {
        let (mut app, _spaces) = fixture(b'W');
        // White box: a create pending alongside a refresh — the
        // reference's ERROR face washes the refresh before creating.
        {
            let (_, witness) = ctx_with(&mut app, 1);
            let mut cache = EntityCache::new(2, 0, 1, 42, 0);
            cache.set_create_pending();
            cache.set_refresh();
            witness.aoi_map.add(cache);
            witness.add_to_seen(2);
        }
        app.process_witness(1);
        let wire = &app.witnesses[&1].wire;
        assert_eq!(create_of(wire), (2, false, 0, vec![0]));
        assert!(
            !app.witnesses
                .get(&1)
                .expect("the witness")
                .aoi_map
                .find(2)
                .expect("the cache")
                .is_refresh()
        );
    }

    #[test]
    fn the_gone_cache_leaves_on_the_next_pass() {
        let (mut app, _spaces) = fixture(b'W');
        move_entity(&mut app, 2, [3.0, 0.0, 4.0]);
        app.pump_aoi_events(1);
        app.process_witness(1);
        move_entity(&mut app, 2, [3000.0, 0.0, 0.0]);
        app.pump_aoi_events(1);
        {
            let witness = app.witnesses.get(&1).expect("the witness");
            let cache = witness.aoi_map.find(2).expect("the gone cache");
            assert!(cache.is_gone());
            assert!(!witness.is_in_aoi(2));
        }
        // The pending cache left the queue with its enter; the leave
        // waits for the client's ack — the reference carries no timer
        // on pending entities.
        app.request_entity_update(1, 2, &[]);
        app.process_witness(1);
        let wire = &app.witnesses[&1].wire;
        // The create never ran, so the stamps are the reset zeros.
        assert!(matches!(
            &wire[2],
            Wire::LeaveAoI { id: 2, lod_stamps } if lod_stamps == &vec![0]
        ));
        assert!(app.witnesses[&1].aoi_map.find(2).is_none());
        assert_eq!(app.witnesses[&1].aoi_map.num_in_aoi, 0);
        assert!(app.script_calls.iter().any(|call| call.name == "onLeftAoI"));

        // A cache that never heard the enter leaves silently.
        let (mut app, _spaces) = fixture(b'W');
        move_entity(&mut app, 2, [3.0, 0.0, 4.0]);
        app.pump_aoi_events(1);
        {
            let (mut ctx, witness) = ctx_with(&mut app, 1);
            let cache = witness.aoi_map.find_mut(2).expect("the cache");
            cache.set_gone();
            cache.set_enter_pending();
            let mut wire = Vec::new();
            witness.delete_from_client(&mut ctx, &mut wire, 2);
            assert!(wire.is_empty());
            witness.delete_from_seen(&mut ctx, &mut wire, 2);
        }
        assert!(app.witnesses[&1].aoi_map.find(2).is_none());
    }

    #[test]
    fn the_withheld_cache_leaves_and_reenters() {
        let (mut app, _spaces) = fixture(b'W');
        move_entity(&mut app, 2, [3.0, 0.0, 4.0]);
        app.pump_aoi_events(1);
        app.process_witness(1);
        app.request_entity_update(1, 2, &[5]);
        app.process_witness(1);
        assert!(
            app.witnesses
                .get(&1)
                .expect("the witness")
                .aoi_map
                .find(2)
                .expect("the cache")
                .is_updatable()
        );

        assert!(
            app.witnesses
                .get_mut(&1)
                .expect("the witness")
                .withhold_from_client(2, true)
        );
        assert_eq!(app.witnesses[&1].entities_withheld(), vec![2]);
        app.process_witness(1);
        let wire = &app.witnesses[&1].wire;
        assert!(matches!(&wire[3], Wire::LeaveAoI { id: 2, .. }));
        {
            let cache = app.witnesses[&1].aoi_map.find(2).expect("the cache");
            assert!(cache.is_withheld());
            assert!(cache.is_enter_pending());
        }

        // Still withheld: the pass holds the enter back — the state
        // machine only re-enters once the withhold lifts.
        app.process_witness(1);
        assert_eq!(app.witnesses[&1].wire.len(), 4);

        app.witnesses
            .get_mut(&1)
            .expect("the witness")
            .withhold_from_client(2, false);
        app.process_witness(1);
        assert_eq!(enter_of(&app.witnesses[&1].wire[4..]), (2, 254));

        // The re-enter leaves the cache request-pending again; the ack
        // finishes the create before the updatable check.
        app.request_entity_update(1, 2, &[5]);
        app.process_witness(1);
        assert!(
            app.witnesses
                .get(&1)
                .expect("the witness")
                .aoi_map
                .find(2)
                .expect("the cache")
                .is_updatable()
        );

        // An updatable cache has no state machine arm: the pass falls
        // straight through.
        {
            let (mut ctx, witness) = ctx_with(&mut app, 1);
            witness.handle_state_change(&mut ctx, 2);
        }
        assert!(
            !app.witnesses
                .get_mut(&1)
                .expect("the witness")
                .withhold_from_client(99, true)
        );
    }

    // ----- The manual AoI faces ---------------------------------------

    #[test]
    fn the_manual_add_survives_the_trigger_leave() {
        let (mut app, _spaces) = fixture(b'W');
        let _ = observe_once(&mut app);
        // Added by hand on top of the trigger add.
        {
            let entity = app.entity_population[&2].clone();
            let (mut ctx, witness) = ctx_with(&mut app, 1);
            assert!(witness.add_to_manual_aoi(&mut ctx, &entity));
            let cache = witness.aoi_map.find(2).expect("the cache");
            assert!(cache.is_manually_added());
            assert!(cache.is_added_by_trigger());
        }

        // The trigger leave only strips the trigger flag.
        move_entity(&mut app, 2, [3000.0, 0.0, 0.0]);
        app.pump_aoi_events(1);
        {
            let cache = app.witnesses[&1].aoi_map.find(2).expect("the cache");
            assert!(!cache.is_gone());
            assert!(cache.is_manually_added());
            assert!(!cache.is_added_by_trigger());
        }

        // The hand removal is what retires it.
        {
            let entity = app.entity_population[&2].clone();
            let (mut ctx, witness) = ctx_with(&mut app, 1);
            assert!(witness.remove_from_manual_aoi(&mut ctx, &entity));
        }
        app.process_witness(1);
        assert!(app.witnesses[&1].aoi_map.find(2).is_none());
        assert!(app.witnesses[&1].entities_in_manual_aoi().is_empty());
    }

    #[test]
    fn the_manual_aoi_faces_refuse_the_wrong_states() {
        let (mut app, _spaces) = fixture(b'W');
        let _ = observe_once(&mut app);

        {
            let entity = app.entity_population[&2].clone();
            let (mut ctx, witness) = ctx_with(&mut app, 1);
            // The first hand add takes (the trigger owns it too).
            assert!(witness.add_to_manual_aoi(&mut ctx, &entity));
            // A second one is refused — already manually known.
            assert!(!witness.add_to_manual_aoi(&mut ctx, &entity));
            assert_eq!(
                ctx.warnings.pop(),
                Some(WitnessNote::AlreadyInAoI { entity: 2 })
            );
            // The hand remove strips the flag; the trigger still
            // holds the entity on the client.
            assert!(witness.remove_from_manual_aoi(&mut ctx, &entity));
            // No longer manually held — the not-manual refusal.
            assert!(!witness.remove_from_manual_aoi(&mut ctx, &entity));
            assert_eq!(
                ctx.warnings.pop(),
                Some(WitnessNote::NotManualAoI { entity: 2 })
            );
            // Gone now — the gone refusal.
            witness.remove_from_aoi(&mut ctx, &entity, false);
            assert!(!witness.remove_from_manual_aoi(&mut ctx, &entity));
            assert_eq!(
                ctx.warnings.pop(),
                Some(WitnessNote::RemoveNotInManualAoI { entity: 2 })
            );
            // Absent entirely.
            let mut stranger = Entity::new(30, true);
            stranger.id = 30;
            let stranger = Rc::new(RefCell::new(stranger));
            assert!(!witness.remove_from_manual_aoi(&mut ctx, &stranger));
            assert_eq!(
                ctx.warnings.pop(),
                Some(WitnessNote::RemoveNotInManualAoI { entity: 30 })
            );
            // Destroyed.
            stranger.borrow_mut().destroyed = true;
            assert!(!witness.add_to_manual_aoi(&mut ctx, &stranger));
            assert_eq!(
                ctx.warnings.pop(),
                Some(WitnessNote::AddDestroyedEntity { entity: 30 })
            );
        }

        // The CRITICAL face: removing what never joined.
        {
            let mut stranger = Entity::new(31, true);
            stranger.id = 31;
            let stranger = Rc::new(RefCell::new(stranger));
            let (mut ctx, witness) = ctx_with(&mut app, 1);
            witness.remove_from_aoi(&mut ctx, &stranger, false);
        }
        assert_eq!(
            app.witness_errors.pop(),
            Some(WitnessNote::RemoveNotInAoI {
                entity: 31,
                witness: 1,
            })
        );
    }

    #[test]
    fn the_manual_aoi_update_syncs_the_list() {
        let (mut app, mut spaces) = fixture(b'W');
        app.entity_scripts
            .insert(11, EntityScript::with_init(vec![]));
        app.create_real_entity(&mut spaces, SID, &stream_for(3, 11, [1.0, 0.0, 1.0], b'-'));
        app.entity_type_faces.insert(
            11,
            EntityTypeDef {
                manual_aoi: true,
                ..EntityTypeDef::default()
            },
        );

        // Entity 2 joins by hand; entity 3 is the manual type.
        {
            let entity = app.entity_population[&2].clone();
            let (mut ctx, witness) = ctx_with(&mut app, 1);
            assert!(witness.add_to_manual_aoi(&mut ctx, &entity));
        }
        app.process_witness(1);
        // Finish 2's observation so its later gone rides the queue.
        app.request_entity_update(1, 2, &[5]);
        app.process_witness(1);

        // A strict update with a non-manual member refuses; the
        // tolerant one warns and proceeds, adding 3 beside 2.
        {
            let (mut ctx, witness) = ctx_with(&mut app, 1);
            assert!(!witness.update_manual_aoi(&mut ctx, &[2, 3], false));
            assert!(witness.update_manual_aoi(&mut ctx, &[2, 3], true));
            assert_eq!(
                ctx.warnings.pop(),
                Some(WitnessNote::NotManualAoIType { entity: 2 })
            );
            assert_eq!(witness.entities_in_manual_aoi(), vec![2, 3]);
        }
        app.process_witness(1);
        app.request_entity_update(1, 3, &[5]);
        app.process_witness(1);

        // Dropping 2 from the list retires it only when its type is a
        // manual one — 2 is not, so it survives until the hand removal.
        {
            let entity = app.entity_population[&2].clone();
            let (mut ctx, witness) = ctx_with(&mut app, 1);
            assert!(witness.update_manual_aoi(&mut ctx, &[3], true));
            assert_eq!(witness.entities_in_manual_aoi(), vec![2, 3]);
            assert!(!witness.update_manual_aoi(&mut ctx, &[40], true));
            assert!(witness.remove_from_manual_aoi(&mut ctx, &entity));
        }
        app.process_witness(1);
        let wire = &app.witnesses[&1].wire;
        assert!(matches!(&wire[1], Wire::EnterAoI { id: 2, .. }));
        assert!(matches!(&wire[2], Wire::CreateEntity { id: 2, .. }));
        assert!(matches!(&wire[3], Wire::EnterAoI { id: 3, .. }));
        assert!(matches!(&wire[4], Wire::CreateEntity { id: 3, .. }));
        assert!(matches!(&wire[5], Wire::LeaveAoI { id: 2, .. }));
        assert_eq!(app.witnesses[&1].entities_in_manual_aoi(), vec![3]);

        // An empty list retires the manual members — 3 is a manual
        // type, so its hand add is undone.
        {
            let (mut ctx, witness) = ctx_with(&mut app, 1);
            assert!(witness.update_manual_aoi(&mut ctx, &[], true));
            assert!(witness.entities_in_manual_aoi().is_empty());
        }
        app.process_witness(1);
        assert!(matches!(
            &app.witnesses[&1].wire[6],
            Wire::LeaveAoI { id: 3, .. }
        ));

        // A stale cache whose entity left the population is skipped.
        {
            let (_, witness) = ctx_with(&mut app, 1);
            witness.aoi_map.add(EntityCache::new(50, 0, 1, 0, 0));
        }
        {
            let (mut ctx, witness) = ctx_with(&mut app, 1);
            assert!(witness.update_manual_aoi(&mut ctx, &[], true));
            assert!(witness.aoi_map.find(50).is_some());
        }
    }

    // ----- The query and configuration faces --------------------------

    #[test]
    fn the_queries_report_visibility() {
        let (mut app, _spaces) = fixture(b'W');
        assert!(!app.witnesses[&1].is_in_aoi(2));
        let _ = observe_once(&mut app);
        assert!(app.witnesses[&1].is_in_aoi(2));
        assert!(!app.witnesses[&1].is_in_aoi(99));
        assert_eq!(app.witnesses[&1].entities_visible_to_client(), vec![2]);

        {
            let witness = app.witnesses.get_mut(&1).expect("the witness");
            assert!(witness.set_position_detailed(2, true));
            assert!(
                witness
                    .aoi_map
                    .find(2)
                    .expect("the cache")
                    .is_always_detailed()
            );
            assert!(!witness.set_position_detailed(99, true));
        }
    }

    #[test]
    fn the_aoi_radius_clamps_and_floors() {
        let (mut app, _spaces) = fixture(b'W');
        assert!(app.set_witness_aoi_radius(1, 600.0, 2.0));
        assert_eq!(app.witnesses[&1].aoi_radius, 500.0);
        assert_eq!(app.witnesses[&1].aoi_hyst, 2.0);
        assert_eq!(
            app.witness_warnings.pop(),
            Some(WitnessNote::RadiusClamped {
                id: 1,
                radius: 600.0,
                max: 500.0,
            })
        );
        assert!(app.set_witness_aoi_radius(1, 0.05, 1.0));
        assert!((app.witnesses[&1].aoi_radius - 0.1).abs() < 1e-6);
        assert!(app.witness_warnings.is_empty());
        assert!(!app.set_witness_aoi_radius(99, 100.0, 1.0));
    }

    #[test]
    fn the_capacity_is_the_bps_budget_minus_overhead() {
        let mut witness = Witness::new(1, SID, 500.0);
        witness.set_witness_capacity(8000, 10);
        assert_eq!(witness.max_packet_size, 72);
    }

    // ----- The bandwidth faces ----------------------------------------

    #[test]
    fn send_to_client_records_the_message_and_the_deficit() {
        let (mut app, _spaces) = fixture(b'W');
        let _ = observe_once(&mut app);
        {
            let (mut ctx, witness) = ctx_with(&mut app, 1);
            assert!(witness.send_to_client(&mut ctx, 2, 77, 100));
        }
        let witness = app.witnesses.get(&1).expect("the witness");
        assert!(matches!(
            witness.wire.last(),
            Some(Wire::SendMessageToClient {
                entity_id: 2,
                message_id: 77,
                size: 100
            })
        ));
        assert_eq!(witness.bandwidth_deficit, 100);
    }

    #[test]
    fn send_to_client_refuses_targets_outside_the_aoi() {
        let (mut app, _spaces) = fixture(b'W');
        move_entity(&mut app, 2, [3.0, 0.0, 4.0]);
        app.pump_aoi_events(1);
        app.process_witness(1);
        {
            let (mut ctx, witness) = ctx_with(&mut app, 1);
            assert!(!witness.send_to_client(&mut ctx, 99, 1, 5));
            // A pending cache is not updatable either.
            assert!(!witness.send_to_client(&mut ctx, 2, 1, 5));
        }
        assert_eq!(
            app.witness_errors.pop(),
            Some(WitnessNote::SelectOutsideAoI {
                witness: 1,
                target: 2,
            })
        );
        assert!(matches!(
            app.witness_errors.pop(),
            Some(WitnessNote::SelectOutsideAoI { target: 99, .. })
        ));
        assert_eq!(app.witnesses[&1].bandwidth_deficit, 0);
    }

    // ----- The request faces ------------------------------------------

    #[test]
    fn the_request_faces_refuse_the_wrong_states() {
        let (mut app, _spaces) = fixture(b'W');
        move_entity(&mut app, 2, [3.0, 0.0, 4.0]);
        app.pump_aoi_events(1);
        app.process_witness(1);
        // Unknown entity: the DEBUG face.
        app.request_entity_update(1, 99, &[]);
        assert_eq!(
            app.witness_warnings.pop(),
            Some(WitnessNote::PendingNoLongerInAoI {
                witness: 1,
                entity: 99,
            })
        );
        // The witness itself: the ERROR face.
        app.request_entity_update(1, 1, &[]);
        assert_eq!(
            app.witness_errors.pop(),
            Some(WitnessNote::RequestForSelf { witness: 1 })
        );
        // The live request: too many stamps clamp to the level count.
        app.request_entity_update(1, 2, &[1, 2, 3, 4]);
        assert_eq!(
            app.witness_errors.pop(),
            Some(WitnessNote::StampsBeyondLevels {
                witness: 1,
                size: 4,
                max: 1,
            })
        );
        assert_eq!(
            app.witnesses[&1]
                .aoi_map
                .find(2)
                .expect("the cache")
                .lod_event_number(0),
            1
        );
        // Consumed — a second request is the not-pending face.
        app.request_entity_update(1, 2, &[5]);
        assert_eq!(
            app.witness_errors.pop(),
            Some(WitnessNote::RequestNotPending {
                witness: 1,
                entity: 2,
            })
        );
    }

    // ----- The vehicle faces ------------------------------------------

    #[test]
    fn the_vehicle_enter_rides_and_the_stale_change_sends() {
        let (mut app, _spaces) = fixture(b'W');
        {
            let mut entity = app.entity_population[&2].borrow_mut();
            entity.vehicle = Some(5);
            entity.vehicle_change_num = 1;
        }
        move_entity(&mut app, 2, [3.0, 0.0, 4.0]);
        app.pump_aoi_events(1);
        app.process_witness(1);
        assert!(matches!(
            &app.witnesses[&1].wire[1],
            Wire::EnterAoIOnVehicle {
                id: 2,
                vehicle_id: 5,
                id_alias: 254
            }
        ));

        // The mount changed again before the client acked.
        {
            let mut entity = app.entity_population[&2].borrow_mut();
            entity.vehicle = Some(6);
            entity.vehicle_change_num = 2;
        }
        app.request_entity_update(1, 2, &[5]);
        app.process_witness(1);
        let wire = &app.witnesses[&1].wire;
        assert!(matches!(
            &wire[2],
            Wire::SetVehicle {
                passenger_id: 2,
                vehicle_id: 6,
                change_num: 2
            }
        ));
        assert_eq!(create_of(&wire[2..]).0, 2);
    }

    // ----- The setup and teardown faces -------------------------------

    #[test]
    fn the_witness_byte_builds_the_witness() {
        let (mut app, _spaces) = fixture(b'-');
        assert!(app.witnesses.is_empty());
        assert_eq!(app.add_witness(SID, 99), Err(WitnessSetup::UnknownEntity));
        assert_eq!(app.add_witness(SID, 1), Err(WitnessSetup::NoWitness));
        let mut bare = Entity::new(40, true);
        bare.has_witness = true;
        app.entity_population
            .insert(40, Rc::new(RefCell::new(bare)));
        assert_eq!(app.add_witness(SID, 40), Err(WitnessSetup::NoRangeNode));

        // The absent-witness faces are no-ops.
        app.pump_aoi_events(9);
        app.process_witness(9);
        app.request_entity_update(9, 1, &[]);
        assert!(!app.remove_witness(9));
        // The b'-' fixture never wired one for 1 either.
        assert!(!app.remove_witness(1));

        // The wired path: teardown then re-add — the arena slot freed.
        let (mut app, _spaces) = fixture(b'W');
        assert!(app.remove_witness(1));
        assert_eq!(app.add_witness(SID, 1), Ok(()));
        assert!(app.witnesses.contains_key(&1));
    }

    #[test]
    fn remove_witness_tolerates_missing_pieces() {
        // No trigger wired: the teardown is a bare take, and the
        // radius change skips the trigger it does not have.
        let (mut app, _spaces) = fixture(b'W');
        app.witnesses.get_mut(&1).expect("the witness").aoi_trigger = None;
        assert!(app.set_witness_aoi_radius(1, 100.0, 1.0));
        assert!((app.witnesses[&1].aoi_radius - 100.0).abs() < 1e-6);
        assert!(app.remove_witness(1));
        assert!(app.witnesses.is_empty());

        // The entity already left the population.
        let (mut app, _spaces) = fixture(b'W');
        app.entity_population.remove(&1);
        assert!(app.remove_witness(1));

        // The space's range list is gone.
        let (mut app, _spaces) = fixture(b'W');
        app.range_lists.clear();
        assert!(app.remove_witness(1));
    }

    #[test]
    fn the_destroy_tears_the_trigger_down() {
        let (mut app, mut spaces) = fixture(b'W');
        move_entity(&mut app, 2, [3.0, 0.0, 4.0]);
        app.pump_aoi_events(1);
        let before = app.range_lists[&SID].x_order().len();
        app.destroy_entity(&mut spaces, 1);
        assert!(app.witnesses.is_empty());
        let entity = app.entity_population[&1].clone();
        assert!(entity.borrow().range_triggers.is_empty());
        // The entity node plus the trigger's two endpoints.
        assert_eq!(app.range_lists[&SID].x_order().len(), before - 3);
    }

    // ----- The noise and the queue ------------------------------------

    #[test]
    fn the_noise_made_falls_or_runs_then_propagates() {
        let mut witness = Witness::new(1, SID, 500.0);
        // Within the same tick the velocity is not sampled.
        witness.new_position(1, 0.5, 3.0, 25.0, [0.0, 0.0, 0.0]);
        assert!(!witness.noise_made);
        // A fall past the vertical speed makes the noise.
        witness.new_position(2, 0.5, 3.0, 25.0, [0.0, -5.0, 0.0]);
        assert!(witness.noise_made);
        assert_eq!(witness.noise_check_time, 2);
        // Ten ticks have not passed yet.
        witness.new_position(5, 0.5, 3.0, 25.0, [0.0, 0.0, 0.0]);
        assert!(witness.noises.is_empty());
        // The propagation nominates the real entity and resets.
        witness.new_position(13, 0.5, 3.0, 25.0, [0.0, 0.0, 0.0]);
        assert_eq!(witness.noises, vec![(0.5, 0)]);
        assert!(!witness.noise_made);
        assert_eq!(witness.noise_propagated_time, 13);

        // The horizontal branch: running past the squared speed.
        let mut runner = Witness::new(2, SID, 500.0);
        runner.new_position(2, 0.25, 3.0, 25.0, [10.0, 0.0, 0.0]);
        assert!(runner.noise_made);

        // Neither: a slow walk makes nothing.
        let mut walker = Witness::new(3, SID, 500.0);
        walker.new_position(2, 0.25, 3.0, 25.0, [1.0, 1.0, 1.0]);
        assert!(!walker.noise_made);
    }

    #[test]
    fn the_heap_sends_in_ascending_priority() {
        let mut witness = Witness::new(1, SID, 500.0);
        for (id, priority) in [(2, 5.0), (3, 1.0), (4, 3.0)] {
            let mut cache = EntityCache::new(id, 0, 1, 0, 0);
            cache.priority = priority;
            witness.aoi_map.add(cache);
            witness.push_seen(id);
        }
        assert_eq!(witness.entity_queue[0], 3);
        assert_eq!(witness.pop_seen(), Some(3));
        assert_eq!(witness.pop_seen(), Some(4));
        assert_eq!(witness.pop_seen(), Some(2));
        assert_eq!(witness.pop_seen(), None);
    }

    #[test]
    fn add_to_seen_takes_the_front_priority() {
        let mut witness = Witness::new(1, SID, 500.0);
        let mut front = EntityCache::new(2, 0, 1, 0, 0);
        front.priority = 6.5;
        witness.aoi_map.add(front);
        witness.push_seen(2);
        witness.aoi_map.add(EntityCache::new(3, 0, 1, 0, 0));
        witness.add_to_seen(3);
        assert!((witness.aoi_map.find(3).expect("the cache").priority - 6.5).abs() < 1e-9);
        let first = witness.pop_seen().expect("a queued entity");
        let second = witness.pop_seen().expect("the other queued entity");
        assert!((first == 2 && second == 3) || (first == 3 && second == 2));

        // An id with no cache behind it still joins the queue.
        witness.add_to_seen(42);
        assert!(witness.entity_queue.contains(&42));
    }

    #[test]
    fn the_delete_from_aoi_keeps_the_heap_ordered() {
        let mut witness = Witness::new(1, SID, 500.0);
        for (id, priority) in [(2, 2.0), (3, 1.0), (4, 3.0), (5, 4.0)] {
            let mut cache = EntityCache::new(id, 0, 1, 0, 0);
            cache.priority = priority;
            witness.aoi_map.add(cache);
            witness.push_seen(id);
        }
        witness.delete_from_aoi(3);
        assert_eq!(witness.pop_seen(), Some(2));
        assert_eq!(witness.pop_seen(), Some(4));
        assert_eq!(witness.pop_seen(), Some(5));
        assert_eq!(witness.pop_seen(), None);
    }

    #[test]
    fn make_seen_heap_restores_a_scrambled_queue() {
        let mut witness = Witness::new(1, SID, 500.0);
        for (id, priority) in [(2, 9.0), (3, 5.0), (4, 1.0), (5, 3.0), (6, 8.0)] {
            let mut cache = EntityCache::new(id, 0, 1, 0, 0);
            cache.priority = priority;
            witness.aoi_map.add(cache);
        }
        // An arbitrary edit scrambles the queue; the delete's restore
        // re-heaps it, right children and swaps included.
        witness.entity_queue = vec![2, 3, 4, 5, 6];
        witness.delete_from_aoi(6);
        assert_eq!(witness.pop_seen(), Some(4));
        assert_eq!(witness.pop_seen(), Some(5));
        assert_eq!(witness.pop_seen(), Some(3));
        assert_eq!(witness.pop_seen(), Some(2));
        assert_eq!(witness.pop_seen(), None);
    }

    #[test]
    fn pop_seen_sifts_towards_the_cheaper_right_child() {
        let mut witness = Witness::new(1, SID, 500.0);
        for (id, priority) in [(2, 1.0), (3, 5.0), (4, 3.0), (5, 8.0)] {
            let mut cache = EntityCache::new(id, 0, 1, 0, 0);
            cache.priority = priority;
            witness.aoi_map.add(cache);
        }
        // The front leaves, the tail lands on the root and descends —
        // the right child is the cheaper one at that node.
        witness.entity_queue = vec![2, 3, 4, 5];
        assert_eq!(witness.pop_seen(), Some(2));
        assert_eq!(witness.pop_seen(), Some(4));
        assert_eq!(witness.pop_seen(), Some(3));
        assert_eq!(witness.pop_seen(), Some(5));
        assert_eq!(witness.pop_seen(), None);
    }

    /// Drives entity `id` through one full observation: in, pumped,
    /// updated, acked, created.
    fn observe(app: &mut CellApp, id: u32, pos: [f32; 3]) {
        move_entity(app, id, pos);
        app.pump_aoi_events(1);
        {
            let (mut ctx, witness) = ctx_with(app, 1);
            witness.update(&mut ctx);
        }
        app.request_entity_update(1, id, &[5]);
        app.process_witness(1);
    }

    /// Three watchers at fixed distances: the create pass rides the
    /// state machine in queue order, then the scheduler repeats the
    /// queue order every pass because the full drain resets the
    /// priorities.
    #[test]
    fn the_fixed_load_sends_the_queue_order_each_pass() {
        let (mut app, mut spaces) = fixture(b'W');
        for (id, pos) in [(3, [9.0, 0.0, 12.0]), (4, [6.0, 0.0, 8.0])] {
            app.create_real_entity(&mut spaces, SID, &stream_for(id, TYPE, pos, b'-'));
        }
        move_entity(&mut app, 3, [9.0, 0.0, 12.0]);
        move_entity(&mut app, 4, [6.0, 0.0, 8.0]);
        move_entity(&mut app, 2, [3.0, 0.0, 4.0]);
        app.pump_aoi_events(1);
        // The enter pass queues nothing for the scheduler.
        let sent = {
            let (mut ctx, witness) = ctx_with(&mut app, 1);
            witness.update(&mut ctx)
        };
        assert!(sent.is_empty());
        for id in [3, 4, 2] {
            app.request_entity_update(1, id, &[5]);
        }
        // The create pass rides the state machine, not the scheduler:
        // the wire gains three creates in the heap's pop order (the
        // three caches sit on the same floor, so the sift decides) and
        // `sent` stays empty.
        {
            let (mut ctx, witness) = ctx_with(&mut app, 1);
            let sent = witness.update(&mut ctx);
            assert!(sent.is_empty());
        }
        {
            let wire = &app.witnesses[&1].wire;
            let tail = &wire[wire.len() - 3..];
            assert!(matches!(&tail[0], Wire::CreateEntity { id: 3, .. }));
            assert!(matches!(&tail[1], Wire::CreateEntity { id: 2, .. }));
            assert!(matches!(&tail[2], Wire::CreateEntity { id: 4, .. }));
        }
        // Now everyone is updatable and the scheduler sequences the
        // sends: highest floor first, the vehicle chain ahead of the
        // pack.
        let sent = {
            let (mut ctx, witness) = ctx_with(&mut app, 1);
            witness.update(&mut ctx)
        };
        assert_eq!(sent, vec![3, 4, 2]);
        // The drain reset every priority, so the next pass runs the
        // same race — with the caches on one floor the heap layout
        // decides the tie, exactly as the reference's std heap does
        // with equal priorities.
        let sent = {
            let (mut ctx, witness) = ctx_with(&mut app, 1);
            witness.update(&mut ctx)
        };
        assert_eq!(sent, vec![3, 2, 4]);
    }

    /// A tight budget stops the pass mid-queue: the tail keeps the
    /// earliest priority and the sent ones never drop below it.
    #[test]
    fn the_budget_stops_the_pass_and_the_tail_keeps_the_floor() {
        let (mut app, mut spaces) = fixture(b'W');
        for (id, pos) in [(3, [9.0, 0.0, 12.0]), (4, [6.0, 0.0, 8.0])] {
            app.create_real_entity(&mut spaces, SID, &stream_for(id, TYPE, pos, b'-'));
        }
        move_entity(&mut app, 3, [9.0, 0.0, 12.0]);
        move_entity(&mut app, 4, [6.0, 0.0, 8.0]);
        move_entity(&mut app, 2, [3.0, 0.0, 4.0]);
        app.pump_aoi_events(1);
        {
            let (mut ctx, witness) = ctx_with(&mut app, 1);
            witness.update(&mut ctx);
        }
        for id in [3, 4, 2] {
            app.request_entity_update(1, id, &[5]);
        }
        {
            let (mut ctx, witness) = ctx_with(&mut app, 1);
            witness.update(&mut ctx);
        }
        // 202 bytes of budget against 100-byte sends: two fit, the
        // third never pops (the two spare bytes are the sequence
        // number).
        app.witnesses
            .get_mut(&1)
            .expect("the witness")
            .set_witness_capacity(18_400, 10);
        let prio_of = |app: &CellApp, id: u32| {
            app.witnesses[&1]
                .aoi_map
                .find(id)
                .expect("the cache")
                .priority
        };
        let pass = |app: &mut CellApp| {
            let (mut ctx, witness) = ctx_with(app, 1);
            witness.update(&mut ctx)
        };
        assert_eq!(pass(&mut app), vec![3, 4]);
        // The unsent 2 is the earliest priority left; 3 and 4 only
        // grew by their distance deltas.
        assert!((prio_of(&app, 2) - 0.0).abs() < 1e-6);
        assert!((prio_of(&app, 3) - 4.0).abs() < 1e-6);
        assert!((prio_of(&app, 4) - 3.0).abs() < 1e-6);
    }

    /// The unpaid spend joins the deficit, shrinking the next budget
    /// until the cap bites — and a pass that sends nothing clears it.
    #[test]
    fn the_deficit_shrinks_the_budget_until_the_cap_bites() {
        let (mut app, _spaces) = fixture(b'W');
        let _ = observe_once(&mut app);
        // 72 bytes of budget against 100-byte sends.
        app.witnesses
            .get_mut(&1)
            .expect("the witness")
            .set_witness_capacity(8_000, 10);
        let pass = |app: &mut CellApp| {
            let (mut ctx, witness) = ctx_with(app, 1);
            let sent = witness.update(&mut ctx);
            (sent, app.witnesses[&1].bandwidth_deficit)
        };
        let (sent, deficit) = pass(&mut app);
        assert_eq!(sent.len(), 1);
        assert_eq!(deficit, 28);
        let (_, deficit) = pass(&mut app);
        assert_eq!(deficit, 56);
        let (_, deficit) = pass(&mut app);
        assert_eq!(deficit, 72);
        assert!(matches!(
            app.witness_warnings.pop(),
            Some(WitnessNote::DeficitCapped {
                witness: 1,
                deficit: 84,
                max: 72
            })
        ));
        // The capped budget admits nothing, so the pass spends
        // nothing and the deficit clears.
        let (sent, deficit) = pass(&mut app);
        assert!(sent.is_empty());
        assert_eq!(deficit, 0);
    }

    /// The mount stack is prioritised before the queue: it sends
    /// first whatever its priority, and a mount the loop never pops
    /// still loses the flag on the way out.
    #[test]
    fn the_mount_stack_sends_first_and_clears_its_flag() {
        let (mut app, mut spaces) = fixture(b'W');
        app.create_real_entity(
            &mut spaces,
            SID,
            &stream_for(3, TYPE, [6.0, 0.0, 8.0], b'-'),
        );
        let _ = observe_once(&mut app);
        observe(&mut app, 3, [6.0, 0.0, 8.0]);
        app.witnesses
            .get_mut(&1)
            .expect("the witness")
            .aoi_map
            .find_mut(3)
            .expect("the cache")
            .set_priority(10.0);
        app.entity_population[&1].borrow_mut().vehicle = Some(3);

        let sent = {
            let (mut ctx, witness) = ctx_with(&mut app, 1);
            witness.update(&mut ctx)
        };
        // The mount rode first; 2 was the only queue pop — 10 is past
        // the front-plus-delta gate.
        assert_eq!(sent, vec![3, 2]);
        let witness = &app.witnesses[&1];
        assert!(!witness.aoi_map.find(3).expect("the cache").is_prioritised());
        // The tail clamp lifted 2 to the mount's floor.
        assert!((witness.aoi_map.find(2).expect("the cache").priority - 10.0).abs() < 1e-6);
    }

    /// A mount outside the AoI reports and the pass moves on.
    #[test]
    fn the_update_reports_a_mount_outside_the_aoi() {
        let (mut app, _spaces) = fixture(b'W');
        app.entity_population[&1].borrow_mut().vehicle = Some(999);
        let sent = {
            let (mut ctx, witness) = ctx_with(&mut app, 1);
            witness.update(&mut ctx)
        };
        assert!(sent.is_empty());
        assert_eq!(
            app.witness_errors.pop(),
            Some(WitnessNote::PrioritiseOutsideAoI {
                witness: 1,
                entity: 999
            })
        );
    }

    /// The spam guard fires on a send bigger than `entitySpamSize`,
    /// and the queue tail keeps the underflow watch.
    #[test]
    fn the_spam_guard_and_the_underflow_watch() {
        let (mut app, _spaces) = fixture(b'W');
        let _ = observe_once(&mut app);
        app.witness_update_bytes = 300;
        {
            let (mut ctx, witness) = ctx_with(&mut app, 1);
            let sent = witness.update(&mut ctx);
            assert_eq!(sent, vec![2]);
        }
        assert_eq!(
            app.witness_warnings.pop(),
            Some(WitnessNote::UpdateSpam {
                entity: 2,
                added: 300,
                witness: 1
            })
        );

        // Two infinities pin the gate shut — the tail watch is the
        // only face that fires.
        let (mut app, _spaces) = fixture(b'W');
        for id in [2, 3] {
            app.entity_population
                .insert(id, Rc::new(RefCell::new(Entity::new(id, true))));
            let mut cache = EntityCache::new(id, 0, 1, 0, 0);
            cache.priority = f64::NEG_INFINITY;
            app.witnesses
                .get_mut(&1)
                .expect("the witness")
                .aoi_map
                .add(cache);
        }
        app.witnesses.get_mut(&1).expect("the witness").entity_queue = vec![2, 3];
        let sent = {
            let (mut ctx, witness) = ctx_with(&mut app, 1);
            witness.update(&mut ctx)
        };
        assert!(sent.is_empty());
        assert!(matches!(
            app.witness_errors.pop(),
            Some(WitnessNote::PriorityUnderflow {
                witness: 1,
                entity: 3,
                ..
            })
        ));
    }

    /// A destroyed entity still held by hand is stripped of the hold
    /// right in the pass.
    #[test]
    fn the_pass_releases_a_destroyed_manual_hold() {
        let (mut app, _spaces) = fixture(b'W');
        let _ = observe_once(&mut app);
        {
            let (_, witness) = ctx_with(&mut app, 1);
            let cache = witness.aoi_map.find_mut(2).expect("the cache");
            cache.set_manually_added();
        }
        app.entity_population[&2].borrow_mut().destroyed = true;
        let sent = {
            let (mut ctx, witness) = ctx_with(&mut app, 1);
            witness.update(&mut ctx)
        };
        let cache = app.witnesses[&1].aoi_map.find(2).expect("the cache stays");
        // The trigger still holds after the stripped hold, so the
        // cache keeps its trigger flag and the pass sends as usual.
        assert!(!cache.is_manually_added());
        assert!(cache.is_added_by_trigger());
        assert_eq!(sent, vec![2]);
    }

    /// A prioritised mount the loop pops re-seats through the re-seat
    /// arm — no second send, no tail walk needed.
    #[test]
    fn the_prioritised_mount_pops_back_to_its_floor() {
        let (mut app, mut spaces) = fixture(b'W');
        app.create_real_entity(
            &mut spaces,
            SID,
            &stream_for(3, TYPE, [6.0, 0.0, 8.0], b'-'),
        );
        observe(&mut app, 3, [6.0, 0.0, 8.0]);
        let _ = observe_once(&mut app);
        // The floor order puts the mount at the queue head.
        app.witnesses
            .get_mut(&1)
            .expect("the witness")
            .aoi_map
            .find_mut(3)
            .expect("the cache")
            .set_priority(-1.0);
        app.entity_population[&1].borrow_mut().vehicle = Some(3);

        let sent = {
            let (mut ctx, witness) = ctx_with(&mut app, 1);
            witness.update(&mut ctx)
        };
        // The mount rode first and its pop re-seated it; 2 followed on
        // its own floor.
        assert_eq!(sent, vec![3, 2]);
        let witness = &app.witnesses[&1];
        assert!(!witness.aoi_map.find(3).expect("the cache").is_prioritised());
        assert!(!witness.aoi_map.find(2).expect("the cache").is_prioritised());
    }

    /// A destroyed mount is skipped by the chain, the pop still
    /// releases its manual hold, and the empty-handed walk breaks at
    /// once.
    #[test]
    fn the_destroyed_mount_skips_the_chain_and_the_walk_breaks() {
        let (mut app, mut spaces) = fixture(b'W');
        app.create_real_entity(
            &mut spaces,
            SID,
            &stream_for(3, TYPE, [6.0, 0.0, 8.0], b'-'),
        );
        let _ = observe_once(&mut app);
        observe(&mut app, 3, [6.0, 0.0, 8.0]);
        {
            let (_, witness) = ctx_with(&mut app, 1);
            let cache = witness.aoi_map.find_mut(2).expect("the cache");
            cache.set_manually_added();
            cache.set_priority(-1.0);
        }
        app.entity_population[&2].borrow_mut().destroyed = true;
        app.entity_population[&1].borrow_mut().vehicle = Some(2);
        // A 40-byte send against a 42-byte budget: one send fills the
        // packet to the gate.
        app.witness_update_bytes = 40;
        app.witnesses
            .get_mut(&1)
            .expect("the witness")
            .set_witness_capacity(5_600, 10);

        let sent = {
            let (mut ctx, witness) = ctx_with(&mut app, 1);
            witness.update(&mut ctx)
        };
        // The chain skipped the destroyed mount; the pop released the
        // hold and spent the budget, so the walk broke before it.
        assert_eq!(sent, vec![2]);
        let witness = &app.witnesses[&1];
        let cache = witness.aoi_map.find(2).expect("the cache stays");
        assert!(!cache.is_manually_added());
        assert!(cache.is_added_by_trigger());
        assert!(app.witness_warnings.is_empty());
        assert!(app.witness_errors.is_empty());
    }

    /// A cache-less mount walks out of the pass with a report and a
    /// skip, leaving the real mount's flag to the walk.
    #[test]
    fn the_chain_walk_skips_a_cache_less_mount() {
        let (mut app, mut spaces) = fixture(b'W');
        app.create_real_entity(
            &mut spaces,
            SID,
            &stream_for(3, TYPE, [6.0, 0.0, 8.0], b'-'),
        );
        let _ = observe_once(&mut app);
        observe(&mut app, 3, [6.0, 0.0, 8.0]);
        app.entity_population
            .insert(999, Rc::new(RefCell::new(Entity::new(999, true))));
        app.entity_population[&999].borrow_mut().vehicle = Some(3);
        app.entity_population[&1].borrow_mut().vehicle = Some(999);
        app.witness_update_bytes = 40;
        app.witnesses
            .get_mut(&1)
            .expect("the witness")
            .set_witness_capacity(5_600, 10);

        let sent = {
            let (mut ctx, witness) = ctx_with(&mut app, 1);
            witness.update(&mut ctx)
        };
        // The chain reported 999 and rode 3; the budget gate stopped
        // the loop before the pop, so the walk skipped 999 again and
        // cleared 3's flag itself.
        assert_eq!(sent, vec![3]);
        assert_eq!(
            app.witness_errors.pop(),
            Some(WitnessNote::PrioritiseOutsideAoI {
                witness: 1,
                entity: 999
            })
        );
        assert!(
            !app.witnesses[&1]
                .aoi_map
                .find(3)
                .expect("the cache")
                .is_prioritised()
        );
    }

    /// The tail walk falls through a mount it never prioritised
    /// before clearing the one it did.
    #[test]
    fn the_walk_falls_through_an_unprioritised_mount() {
        let (mut app, mut spaces) = fixture(b'W');
        app.create_real_entity(
            &mut spaces,
            SID,
            &stream_for(3, TYPE, [6.0, 0.0, 8.0], b'-'),
        );
        app.create_real_entity(
            &mut spaces,
            SID,
            &stream_for(4, TYPE, [12.0, 0.0, 16.0], b'-'),
        );
        let _ = observe_once(&mut app);
        observe(&mut app, 3, [6.0, 0.0, 8.0]);
        // 4 rides half-observed: its enter is unacked, so the chain
        // skips it and the walk meets the cache without a flag.
        move_entity(&mut app, 4, [12.0, 0.0, 16.0]);
        app.pump_aoi_events(1);
        app.process_witness(1);
        app.entity_population[&1].borrow_mut().vehicle = Some(4);
        app.entity_population[&4].borrow_mut().vehicle = Some(3);
        app.witness_update_bytes = 40;
        app.witnesses
            .get_mut(&1)
            .expect("the witness")
            .set_witness_capacity(5_600, 10);

        let sent = {
            let (mut ctx, witness) = ctx_with(&mut app, 1);
            witness.update(&mut ctx)
        };
        assert_eq!(sent, vec![3]);
        let witness = &app.witnesses[&1];
        assert!(!witness.aoi_map.find(3).expect("the cache").is_prioritised());
        assert!(
            witness
                .aoi_map
                .find(4)
                .expect("the cache")
                .is_request_pending()
        );
    }

    /// The spam guard rides the chain too: a prioritised mount's send
    /// warns like any other.
    #[test]
    fn the_chain_send_spams_like_the_rest() {
        let (mut app, mut spaces) = fixture(b'W');
        app.create_real_entity(
            &mut spaces,
            SID,
            &stream_for(3, TYPE, [6.0, 0.0, 8.0], b'-'),
        );
        let _ = observe_once(&mut app);
        observe(&mut app, 3, [6.0, 0.0, 8.0]);
        app.witness_update_bytes = 300;
        app.entity_population[&1].borrow_mut().vehicle = Some(3);
        app.witnesses
            .get_mut(&1)
            .expect("the witness")
            .aoi_map
            .find_mut(3)
            .expect("the cache")
            .set_priority(-1.0);

        let sent = {
            let (mut ctx, witness) = ctx_with(&mut app, 1);
            witness.update(&mut ctx)
        };
        assert_eq!(sent, vec![3, 2]);
        assert_eq!(
            app.witness_warnings[0],
            WitnessNote::UpdateSpam {
                entity: 3,
                added: 300,
                witness: 1
            }
        );
        assert_eq!(
            app.witness_warnings[1],
            WitnessNote::UpdateSpam {
                entity: 2,
                added: 300,
                witness: 1
            }
        );
    }

    /// A stale queue id with no cache behind it walks the pass
    /// without a send.
    #[test]
    fn the_pass_walks_past_a_stale_queue_entry() {
        let (mut app, _spaces) = fixture(b'W');
        let _ = observe_once(&mut app);
        // White box: an id queued without a cache behind it — the
        // reference's `deleteFromAoI` swap can leave one.
        app.witnesses
            .get_mut(&1)
            .expect("the witness")
            .add_to_seen(999);
        let sent = {
            let (mut ctx, witness) = ctx_with(&mut app, 1);
            witness.update(&mut ctx)
        };
        // 999 has no cache: nothing to spend and nothing to carry
        // back on the heap.
        assert_eq!(sent, vec![2]);
        assert_eq!(app.witnesses[&1].entity_queue, vec![2]);
        assert!(app.witness_errors.is_empty());
    }

    /// The mount chain walk stops on a cycle instead of looping.
    #[test]
    fn the_mount_chain_walk_breaks_on_a_cycle() {
        let (mut app, mut spaces) = fixture(b'W');
        app.create_real_entity(
            &mut spaces,
            SID,
            &stream_for(3, TYPE, [6.0, 0.0, 8.0], b'-'),
        );
        app.entity_population[&1].borrow_mut().vehicle = Some(3);
        app.entity_population[&3].borrow_mut().vehicle = Some(1);
        let (ctx, witness) = ctx_with(&mut app, 1);
        assert_eq!(witness.vehicle_chain(&ctx), vec![3, 1]);
    }
}
