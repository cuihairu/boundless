//! BW-08-03-02: manage the witness and the entity cache.
//!
//! `Witness` (`witness.cpp`) is the Area of Interest of a real entity: its
//! `AoITrigger` fires `triggerEnter`/`triggerLeave` (`witness.cpp:3586`,
//! `:3602`), which the witness forwards to `addToAoI` (`witness.cpp:2202`) and
//! `removeFromAoI` (`witness.cpp:2305`). Each observed entity gets an
//! `EntityCache` (`entity_cache.cpp`) in `aoiMap_`. `addToAoI` either creates a
//! new cache (with `setEnterPending` and `addToSeen`, `witness.cpp:2202`),
//! reuses a gone one (`EntityCache::reuse`, `entity_cache.cpp:382`), or — when
//! the entity is already in the AoI — only refreshes its source flag and
//! returns. `removeFromAoI` marks the cache `GONE` and fires `onLeftAoI`; a
//! later re-entry is a `reuse` of that same cache, and `isAlwaysDetailed` is
//! reset to the type default each time `onEnteredAoI` fires
//! (`witness.cpp:2202`). `addToSeen` (`witness.cpp:1848`) seeds a new cache
//! with the current minimum priority (the queue front under `PriorityCompare`,
//! `witness.cpp:56`), so it waits its turn.
//!
//! The acceptance, "首次观察和重复进入结果一致", is that the cache state after a
//! first observation equals the state after a leave-then-re-enter: the same
//! cache entry with `onEnteredAoI` fired in both, `isAlwaysDetailed` back at
//! the type default, and the priority scheme unchanged — while a repeated
//! entry that never left is a no-op.
//!
//! BW-08-03-03 adds `Witness::update` (`witness.cpp:1088`): the entities are
//! walked in priority order (the queue front, `witness.cpp:1140`) and sent
//! until the packet budget is spent. Each sent entity accumulates a
//! distance-based priority delta (`updatePriority`, `entity_cache.ipp`), with
//! `AoIUpdateScheme::apply` (`aoi_update_schemes.hpp:40`) turning the squared
//! distance into that delta — closer entities stay near the front and are sent
//! more often. `maxPriority` (`witness.cpp:1135`) caps how much priority the
//! loop walks in one tick, so far entities are never sent every tick; when the
//! whole queue is sent, every priority resets to 0 (`witness.cpp:1300`).
//!
//! The BW-08-03-03 acceptance, "固定负载下发送序列吻合", is that with a fixed set
//! of entities at fixed distances and a fixed budget, the update selects the
//! same sequence every time — closest first, bounded by the budget, capped by
//! `maxPriority`, and reset when fully drained.

use std::collections::BTreeMap;

/// The ground-plane position used for distance and priority.
pub use crate::range_list::Position;
/// `EntityID`, shared with the real entity.
pub use crate::real_entity::EntityId;

/// `NO_ID_ALIAS` (`entity_cache.hpp`): the sentinel id alias, never handed out.
pub const NO_ID_ALIAS: u8 = 0xff;

/// `EntityCache::MAX_LOD_LEVELS` (`entity_cache.hpp`).
pub const MAX_LOD_LEVELS: usize = 4;

/// The `EntityCache::Flags` bit flags (`entity_cache.hpp`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CacheFlags(u16);

impl CacheFlags {
    /// `ENTER_PENDING` (`1 << 0`): waiting to send `enterAoI`.
    pub const ENTER_PENDING: CacheFlags = CacheFlags(1 << 0);
    /// `REQUEST_PENDING` (`1 << 1`): expecting `requestEntityUpdate`.
    pub const REQUEST_PENDING: CacheFlags = CacheFlags(1 << 1);
    /// `CREATE_PENDING` (`1 << 2`): waiting to send `createEntity`.
    pub const CREATE_PENDING: CacheFlags = CacheFlags(1 << 2);
    /// `GONE` (`1 << 3`): waiting to be removed from the priority queue.
    pub const GONE: CacheFlags = CacheFlags(1 << 3);
    /// `WITHHELD` (`1 << 4`): do not send to the client.
    pub const WITHHELD: CacheFlags = CacheFlags(1 << 4);
    /// `REFRESH` (`1 << 5`): waiting to be removed and re-added to the AoI.
    pub const REFRESH: CacheFlags = CacheFlags(1 << 5);
    /// `PRIORITISED` (`1 << 6`): should be sent in the next `Witness::update`.
    pub const PRIORITISED: CacheFlags = CacheFlags(1 << 6);
    /// `MANUALLY_ADDED` (`1 << 7`): added manually to the AoI.
    pub const MANUALLY_ADDED: CacheFlags = CacheFlags(1 << 7);
    /// `ADDED_BY_TRIGGER` (`1 << 8`): added automatically by the trigger.
    pub const ADDED_BY_TRIGGER: CacheFlags = CacheFlags(1 << 8);
    /// `IS_ALWAYS_DETAILED` (`1 << 9`): volatile position updates are detailed
    /// at all distances.
    pub const IS_ALWAYS_DETAILED: CacheFlags = CacheFlags(1 << 9);

    /// `NOT_UPDATABLE`: if any of these are set, no normal update is done.
    const NOT_UPDATABLE: CacheFlags =
        CacheFlags((1 << 0) | (1 << 1) | (1 << 2) | (1 << 3) | (1 << 4) | (1 << 5));
    /// `CLIENT_STATE`: client-related state, reset when removed from client.
    const CLIENT_STATE: CacheFlags =
        CacheFlags((1 << 0) | (1 << 1) | (1 << 2) | (1 << 3) | (1 << 5));

    /// Whether no flag is set.
    pub fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// The raw bits.
    pub fn bits(self) -> u16 {
        self.0
    }

    /// Whether all the flags in `other` are set.
    pub fn contains(self, other: CacheFlags) -> bool {
        self.0 & other.0 == other.0
    }

    fn insert(&mut self, other: CacheFlags) {
        self.0 |= other.0;
    }

    fn remove(&mut self, other: CacheFlags) {
        self.0 &= !other.0;
    }
}

/// An AoI callback, in the order it happened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AoIEvent {
    /// `onEnteredAoI` fired (`witness.cpp:2202`).
    Entered(EntityId),
    /// `onLeftAoI` fired (`witness.cpp:2305`).
    Left(EntityId),
}

/// `EntityCache` (`entity_cache.cpp`): the witness's record of one observed
/// entity — its client state, priority and id alias.
#[derive(Debug, Clone)]
pub struct EntityCache {
    entity: EntityId,
    flags: CacheFlags,
    priority: f64,
    id_alias: u8,
    detail_level: u8,
    last_event_number: u32,
    last_volatile_update_number: u16,
}

impl EntityCache {
    /// A fresh cache for `entity` with its client state reset
    /// (`resetClientState`, `entity_cache.ipp`).
    pub fn new(entity: EntityId) -> Self {
        EntityCache {
            entity,
            flags: CacheFlags::default(),
            priority: 0.0,
            id_alias: NO_ID_ALIAS,
            detail_level: 0,
            last_event_number: 0,
            last_volatile_update_number: 0,
        }
    }

    /// The observed entity.
    pub fn entity(&self) -> EntityId {
        self.entity
    }

    /// `resetClientState` (`entity_cache.ipp`): reset the client-related state
    /// to a clean slate — a fresh id alias and detail level.
    pub fn reset_client_state(&mut self) {
        self.flags.remove(CacheFlags::CLIENT_STATE);
        self.last_event_number = 0;
        self.last_volatile_update_number = 0;
        self.detail_level = 0;
        self.id_alias = NO_ID_ALIAS;
    }

    /// `onEntityRemovedFromClient` (`entity_cache.ipp`): the entity left the
    /// client; reset client state but keep the server-side flags.
    pub fn on_entity_removed_from_client(&mut self) {
        self.flags.remove(CacheFlags::CLIENT_STATE);
        self.reset_client_state();
    }

    /// `EntityCache::reuse` (`entity_cache.cpp:382`): re-arm a gone cache for a
    /// re-entry, clearing the `GONE` flag.
    pub fn reuse(&mut self) {
        self.flags.remove(CacheFlags::GONE);
    }

    /// The cache's flags.
    pub fn flags(&self) -> CacheFlags {
        self.flags
    }

    /// The accumulated priority (`priority_`).
    pub fn priority(&self) -> f64 {
        self.priority
    }

    /// Set the priority (`priority_`).
    pub fn set_priority(&mut self, priority: f64) {
        self.priority = priority;
    }

    /// The allocated id alias (`idAlias_`), `NO_ID_ALIAS` when none.
    pub fn id_alias(&self) -> u8 {
        self.id_alias
    }

    /// Set the id alias.
    pub fn set_id_alias(&mut self, id_alias: u8) {
        self.id_alias = id_alias;
    }

    /// The current detail level (`detailLevel_`).
    pub fn detail_level(&self) -> u8 {
        self.detail_level
    }

    /// Set the detail level.
    pub fn set_detail_level(&mut self, detail_level: u8) {
        self.detail_level = detail_level;
    }

    /// The last event number sent (`lastEventNumber_`).
    pub fn last_event_number(&self) -> u32 {
        self.last_event_number
    }

    /// Set the last event number sent.
    pub fn set_last_event_number(&mut self, number: u32) {
        self.last_event_number = number;
    }

    fn set(&mut self, flag: CacheFlags) {
        self.flags.insert(flag);
    }

    fn clear(&mut self, flag: CacheFlags) {
        self.flags.remove(flag);
    }

    fn is(&self, flag: CacheFlags) -> bool {
        self.flags.contains(flag)
    }

    /// `isGone`.
    pub fn is_gone(&self) -> bool {
        self.is(CacheFlags::GONE)
    }

    /// `setGone`.
    pub fn set_gone(&mut self) {
        self.set(CacheFlags::GONE);
    }

    /// `isEnterPending`.
    pub fn is_enter_pending(&self) -> bool {
        self.is(CacheFlags::ENTER_PENDING)
    }

    /// `setEnterPending`.
    pub fn set_enter_pending(&mut self) {
        self.set(CacheFlags::ENTER_PENDING);
    }

    /// `clearEnterPending`.
    pub fn clear_enter_pending(&mut self) {
        self.clear(CacheFlags::ENTER_PENDING);
    }

    /// `isCreatePending`.
    pub fn is_create_pending(&self) -> bool {
        self.is(CacheFlags::CREATE_PENDING)
    }

    /// `setCreatePending`.
    pub fn set_create_pending(&mut self) {
        self.set(CacheFlags::CREATE_PENDING);
    }

    /// `isRefresh`.
    pub fn is_refresh(&self) -> bool {
        self.is(CacheFlags::REFRESH)
    }

    /// `setRefresh`.
    pub fn set_refresh(&mut self) {
        self.set(CacheFlags::REFRESH);
    }

    /// `isManuallyAdded`.
    pub fn is_manually_added(&self) -> bool {
        self.is(CacheFlags::MANUALLY_ADDED)
    }

    /// `setManuallyAdded`.
    pub fn set_manually_added(&mut self) {
        self.set(CacheFlags::MANUALLY_ADDED);
    }

    /// `clearManuallyAdded`.
    pub fn clear_manually_added(&mut self) {
        self.clear(CacheFlags::MANUALLY_ADDED);
    }

    /// `isAddedByTrigger`.
    pub fn is_added_by_trigger(&self) -> bool {
        self.is(CacheFlags::ADDED_BY_TRIGGER)
    }

    /// `setAddedByTrigger`.
    pub fn set_added_by_trigger(&mut self) {
        self.set(CacheFlags::ADDED_BY_TRIGGER);
    }

    /// `clearAddedByTrigger`.
    pub fn clear_added_by_trigger(&mut self) {
        self.clear(CacheFlags::ADDED_BY_TRIGGER);
    }

    /// `isAlwaysDetailed` (`IS_ALWAYS_DETAILED`).
    pub fn is_always_detailed(&self) -> bool {
        self.is(CacheFlags::IS_ALWAYS_DETAILED)
    }

    /// `setAlwaysDetailed`.
    pub fn set_always_detailed(&mut self, value: bool) {
        if value {
            self.set(CacheFlags::IS_ALWAYS_DETAILED);
        } else {
            self.clear(CacheFlags::IS_ALWAYS_DETAILED);
        }
    }

    /// `isUpdatable`: no `NOT_UPDATABLE` flag is set.
    pub fn is_updatable(&self) -> bool {
        !self.flags.contains(CacheFlags::NOT_UPDATABLE)
    }
}

/// The outcome of [`Witness::add_to_aoi`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddOutcome {
    /// A new cache was created and queued (first observation).
    Added,
    /// A gone cache was re-armed and the entity re-added (re-entry).
    Reused,
    /// The entity was already in the AoI; only its source flag was refreshed.
    AlreadyPresent,
}

/// The outcome of [`Witness::remove_from_aoi`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoveOutcome {
    /// The cache was marked gone and `onLeftAoI` fired.
    Removed,
    /// The other source still holds the entity in the AoI.
    StillHeld,
    /// No cache existed for the entity.
    NotFound,
}

/// `witnessUpdateMaxPriorityDelta` (`cellapp_config.cpp:87`): the cap on how
/// much priority the update loop walks in one tick.
pub const MAX_PRIORITY_DELTA: f64 = 5.0;

/// `AoIUpdateScheme` (`aoi_update_schemes.hpp:40`): the priority delta for a
/// given distance. With the defaults `minDelta = 1` and `maxDelta = 101`
/// (`cellapp_config.cpp:88`) over `max_radius`, an entity at `distance` gains
/// `distance * (maxDelta / minDelta - 1) / max_radius + 1` per update.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AoIUpdateScheme {
    /// `minDelta`: the delta for an entity at distance 0.
    pub min_delta: f64,
    /// `maxDelta`: the delta for an entity at `max_radius`.
    pub max_delta: f64,
    /// `max_radius`: the AoI radius the scheme is normalised over.
    pub max_radius: f64,
}

impl AoIUpdateScheme {
    /// The default scheme (`AoIUpdateSchemes::init`, `aoi_update_schemes.cpp`):
    /// `minDelta = 1`, `maxDelta = 101`.
    pub fn default_scheme(max_radius: f64) -> Self {
        AoIUpdateScheme {
            min_delta: 1.0,
            max_delta: 101.0,
            max_radius,
        }
    }

    /// `AoIUpdateScheme::apply` (`aoi_update_schemes.hpp:40`): the priority
    /// delta for an entity `distance_squared` away from the witness.
    pub fn priority_delta(&self, distance_squared: f64) -> f64 {
        let distance = distance_squared.sqrt();
        let distance_weighting = (self.max_delta / self.min_delta - 1.0) / self.max_radius;
        (distance * distance_weighting + 1.0) * self.min_delta
    }
}

/// The result of one [`Witness::update`]: what was sent, and the deficit.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct UpdateResult {
    /// The entities sent a normal update, in priority order.
    pub sent_updates: Vec<EntityId>,
    /// The entities sent an `enterAoI` (`handleEnterPending`).
    pub sent_enters: Vec<EntityId>,
    /// The caches dropped because their entity left (`deleteFromSeen`).
    pub deleted: Vec<EntityId>,
    /// `bandwidthDeficit_` (`witness.cpp:1244`).
    pub bandwidth_deficit: usize,
}

/// The number of bytes one normal update costs in the model (a minimal
/// position update).
const UPDATE_BYTES: usize = 4;

/// `Witness` (`witness.cpp`): the AoI of a real entity — the map of observed
/// entities, the priority queue, and the id alias pool.
#[derive(Debug, Default)]
pub struct Witness {
    /// The owning entity (`owner_`).
    owner: EntityId,
    /// `aoiRadius_`.
    radius: f64,
    /// `maxAoIRadius_` clamp for [`set_radius`](Self::set_radius).
    max_radius: f64,
    /// `aoiMap_`.
    aoi_map: BTreeMap<EntityId, EntityCache>,
    /// `entityQueue_`, ordered so the front holds the minimum priority.
    entity_queue: Vec<EntityId>,
    /// The `onEnteredAoI`/`onLeftAoI` callbacks, in order.
    events: Vec<AoIEvent>,
    /// `freeAliases_`: the pool of unallocated id aliases (a stack).
    free_aliases: Vec<u8>,
}

impl Witness {
    /// A witness for `owner` with AoI `radius`, clamped to `max_radius`. The
    /// alias pool starts with every alias but [`NO_ID_ALIAS`].
    pub fn new(owner: EntityId, radius: f64, max_radius: f64) -> Self {
        Witness {
            owner,
            radius: radius.clamp(0.1, max_radius),
            max_radius,
            aoi_map: BTreeMap::new(),
            entity_queue: Vec::new(),
            events: Vec::new(),
            free_aliases: (0..NO_ID_ALIAS).collect(),
        }
    }

    /// The owning entity.
    pub fn owner(&self) -> EntityId {
        self.owner
    }

    /// The AoI radius.
    pub fn radius(&self) -> f64 {
        self.radius
    }

    /// `setAoIRadius` (`witness.cpp`): set the radius, clamped to `0.1` and to
    /// `max_radius`.
    pub fn set_radius(&mut self, radius: f64) {
        self.radius = radius.clamp(0.1, self.max_radius);
    }

    /// The observed-entity map (`aoiMap_`).
    pub fn caches(&self) -> &BTreeMap<EntityId, EntityCache> {
        &self.aoi_map
    }

    /// The cache for `entity`, if it is being observed.
    pub fn cache(&self, entity: EntityId) -> Option<&EntityCache> {
        self.aoi_map.get(&entity)
    }

    /// Mutable access to the cache for `entity`.
    pub fn cache_mut(&mut self, entity: EntityId) -> Option<&mut EntityCache> {
        self.aoi_map.get_mut(&entity)
    }

    /// Whether `entity` is currently in the AoI (cached and not gone).
    pub fn inside(&self, entity: EntityId) -> bool {
        self.aoi_map
            .get(&entity)
            .is_some_and(|cache| !cache.is_gone())
    }

    /// The priority queue (`entityQueue_`), front first.
    pub fn entity_queue(&self) -> &[EntityId] {
        &self.entity_queue
    }

    /// The `onEnteredAoI`/`onLeftAoI` callbacks, in order.
    pub fn events(&self) -> &[AoIEvent] {
        &self.events
    }

    /// The number of observed entities.
    pub fn len(&self) -> usize {
        self.aoi_map.len()
    }

    /// Whether nothing is observed.
    pub fn is_empty(&self) -> bool {
        self.aoi_map.is_empty()
    }

    /// `Witness::addToAoI` (`witness.cpp:2202`): observe `entity`, creating a
    /// new cache, reusing a gone one, or just refreshing the source flag.
    pub fn add_to_aoi(&mut self, entity: EntityId, manually_added: bool) -> AddOutcome {
        if let Some(cache) = self.aoi_map.get_mut(&entity) {
            if cache.is_gone() {
                // `EntityCache::reuse` re-arms the same cache for the re-entry.
                cache.reuse();
                self.finish_add(entity, manually_added);
                return AddOutcome::Reused;
            }
            // Already in the AoI: only refresh the source flag.
            if manually_added {
                cache.set_manually_added();
            } else {
                cache.set_added_by_trigger();
            }
            return AddOutcome::AlreadyPresent;
        }

        // First observation: a fresh cache, enter pending, then queued.
        let mut cache = EntityCache::new(entity);
        cache.set_enter_pending();
        self.aoi_map.insert(entity, cache);
        self.add_to_seen(entity);
        self.finish_add(entity, manually_added);
        AddOutcome::Added
    }

    /// The tail shared by a first observation and a reuse: mark the source,
    /// reset `isAlwaysDetailed` to the type default, and fire `onEnteredAoI`
    /// (`witness.cpp:2202`).
    fn finish_add(&mut self, entity: EntityId, manually_added: bool) {
        let cache = self.aoi_map.get_mut(&entity).expect("cache exists");
        if manually_added {
            cache.set_manually_added();
        } else {
            cache.set_added_by_trigger();
        }
        cache.set_always_detailed(false);
        self.events.push(AoIEvent::Entered(entity));
    }

    /// `Witness::addToSeen` (`witness.cpp:1848`): seed the new cache with the
    /// current minimum priority (the queue front) and push it onto the queue.
    fn add_to_seen(&mut self, entity: EntityId) {
        self.sort_queue();
        let initial = self
            .entity_queue
            .first()
            .and_then(|&front| self.aoi_map.get(&front))
            .map(EntityCache::priority)
            .unwrap_or(0.0);
        if let Some(cache) = self.aoi_map.get_mut(&entity) {
            cache.set_priority(initial);
        }
        self.entity_queue.push(entity);
        self.sort_queue();
    }

    /// Re-order the queue so the front holds the minimum priority
    /// (`PriorityCompare`, `witness.cpp:56`; the `std::make_heap` of
    /// `Witness::update`).
    fn sort_queue(&mut self) {
        let mut indexed: Vec<(EntityId, f64)> = self
            .entity_queue
            .iter()
            .map(|&e| (e, self.cache_priority(e)))
            .collect();
        indexed.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
        self.entity_queue = indexed.into_iter().map(|(e, _)| e).collect();
    }

    fn cache_priority(&self, entity: EntityId) -> f64 {
        self.aoi_map
            .get(&entity)
            .map(EntityCache::priority)
            .unwrap_or(0.0)
    }

    /// `Witness::removeFromAoI` (`witness.cpp:2305`): stop observing `entity`.
    pub fn remove_from_aoi(
        &mut self,
        entity: EntityId,
        clear_manually_added: bool,
    ) -> RemoveOutcome {
        let Some(cache) = self.aoi_map.get_mut(&entity) else {
            return RemoveOutcome::NotFound;
        };
        if clear_manually_added {
            cache.clear_manually_added();
            if cache.is_added_by_trigger() {
                return RemoveOutcome::StillHeld;
            }
        } else {
            cache.clear_added_by_trigger();
            if cache.is_manually_added() {
                return RemoveOutcome::StillHeld;
            }
        }
        cache.set_gone();
        self.events.push(AoIEvent::Left(entity));
        RemoveOutcome::Removed
    }

    /// `allocateIDAlias` (`witness.cpp:1874`): hand out a free alias, or
    /// [`NO_ID_ALIAS`] when the pool is empty.
    pub fn allocate_id_alias(&mut self) -> u8 {
        match self.free_aliases.pop() {
            Some(alias) => alias,
            None => NO_ID_ALIAS,
        }
    }

    /// `onLeaveAoI` (`witness.cpp:2096`): return a used alias to the pool.
    pub fn recycle_id_alias(&mut self, alias: u8) {
        if alias != NO_ID_ALIAS {
            self.free_aliases.push(alias);
        }
    }

    /// `EntityCacheMap::del` (`entity_cache.cpp:65`): drop the cache for a
    /// destroyed entity.
    pub fn delete_cache(&mut self, entity: EntityId) {
        self.aoi_map.remove(&entity);
        self.entity_queue.retain(|&e| e != entity);
    }

    /// `Witness::update` (`witness.cpp:1088`): pick the updates to send this
    /// tick. Entities are walked in priority order (the queue front, `witness.cpp:1140`)
    /// and sent until the packet budget is spent; each sent entity accumulates a
    /// distance-based priority delta (`updatePriority`), so closer entities stay
    /// near the front and are sent more often. `maxPriority`
    /// (`witness.cpp:1135`) caps how much priority the loop walks, so far
    /// entities are never updated at every tick. When the whole queue is sent,
    /// every priority resets to 0 (`witness.cpp:1300`).
    pub fn update(
        &mut self,
        origin: Position,
        budget_bytes: usize,
        scheme: &AoIUpdateScheme,
        positions: &BTreeMap<EntityId, Position>,
    ) -> UpdateResult {
        self.sort_queue();
        let front = self
            .entity_queue
            .first()
            .map_or(0.0, |&e| self.cache_priority(e));
        let max_priority = front + MAX_PRIORITY_DELTA;

        // Walk the queue front-first; the snapshot keeps the selection
        // deterministic for a fixed load.
        let mut order: Vec<EntityId> = self.entity_queue.clone();
        order.sort_by(|a, b| {
            self.cache_priority(*a)
                .partial_cmp(&self.cache_priority(*b))
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        let mut result = UpdateResult::default();
        let mut bytes_used = 0usize;
        let mut all_sent = true;

        for &entity in &order {
            // The loop condition (`witness.cpp:1141`): stop before sending when
            // the packet has no space left.
            if bytes_used >= budget_bytes {
                all_sent = false;
                break;
            }
            let Some(cache) = self.cache_mut(entity) else {
                continue;
            };
            if cache.is_gone() {
                // `deleteFromSeen`: drop the gone cache from the queue.
                result.deleted.push(entity);
                continue;
            }
            if cache.is_enter_pending() {
                // `handleEnterPending`: send `enterAoI`, then it is created
                // later. The enter does not count as a normal update.
                cache.clear_enter_pending();
                result.sent_enters.push(entity);
                continue;
            }
            if !cache.is_updatable() {
                // `CREATE_PENDING`, `WITHHELD` and `REFRESH` are the create and
                // refresh state changes, not the normal update sequence.
                continue;
            }
            if cache.priority() >= max_priority {
                all_sent = false;
                break;
            }

            // `sendQueueElement` + `updatePriority` (`witness.cpp:1043`).
            let distance_squared = positions
                .get(&entity)
                .map(|pos| distance_squared(origin, *pos))
                .unwrap_or(0.0);
            let delta = scheme.priority_delta(distance_squared);
            cache.set_priority(cache.priority() + delta);
            result.sent_updates.push(entity);
            bytes_used += UPDATE_BYTES;
        }

        // `witness.cpp:1300`: if the whole queue was sent, reset every
        // priority so the next tick starts from an even order.
        if all_sent {
            for cache in self.aoi_map.values_mut() {
                cache.set_priority(0.0);
            }
        }

        result.bandwidth_deficit = bytes_used.saturating_sub(budget_bytes);
        result
    }
}

/// `getLoDPriority` (`entity_cache.ipp`): the squared ground-plane distance
/// from `origin` to `pos`.
fn distance_squared(origin: Position, pos: Position) -> f64 {
    let dx = pos.x - origin.x;
    let dz = pos.z - origin.z;
    dx * dx + dz * dz
}

#[cfg(test)]
mod tests {
    use super::*;

    fn witness() -> Witness {
        Witness::new(1, 50.0, 500.0)
    }

    /// The acceptance, first half: a first observation creates the cache with
    /// `onEnteredAoI`, enter pending, and the type-default detail flag.
    #[test]
    fn a_first_observation_creates_the_cache() {
        let mut w = witness();
        assert_eq!(w.add_to_aoi(2, false), AddOutcome::Added);

        let cache = w.cache(2).expect("cache created");
        assert!(cache.is_enter_pending());
        assert!(cache.is_added_by_trigger());
        assert!(!cache.is_manually_added());
        assert!(!cache.is_gone());
        assert!(!cache.is_always_detailed(), "reset to the type default");
        assert_eq!(w.events(), &[AoIEvent::Entered(2)]);
        assert!(w.inside(2));
        assert_eq!(w.len(), 1);
    }

    /// The acceptance, second half: a leave-then-re-enter reuses the same
    /// cache and lands in the same state — the re-entry is consistent with the
    /// first observation.
    #[test]
    fn a_reentry_after_a_leave_is_consistent_with_the_first_observation() {
        let mut w = witness();
        w.add_to_aoi(2, false);
        let first_flags = w.cache(2).unwrap().flags();
        let first_priority = w.cache(2).unwrap().priority();

        assert_eq!(w.remove_from_aoi(2, false), RemoveOutcome::Removed);
        assert!(w.cache(2).unwrap().is_gone());
        assert_eq!(w.events(), &[AoIEvent::Entered(2), AoIEvent::Left(2)]);

        assert_eq!(w.add_to_aoi(2, false), AddOutcome::Reused);
        let cache = w.cache(2).unwrap();
        assert!(!cache.is_gone(), "reuse cleared gone");
        assert!(cache.is_added_by_trigger());
        assert!(!cache.is_always_detailed(), "type default again");
        assert_eq!(cache.flags(), first_flags, "same state as first");
        assert_eq!(
            cache.priority(),
            first_priority,
            "priority scheme unchanged"
        );
        assert_eq!(
            w.events(),
            &[
                AoIEvent::Entered(2),
                AoIEvent::Left(2),
                AoIEvent::Entered(2)
            ]
        );
    }

    /// A repeated entry that never left is a no-op: no new callback, no new
    /// state, just the source flag refreshed.
    #[test]
    fn a_repeated_entry_without_a_leave_is_a_no_op() {
        let mut w = witness();
        w.add_to_aoi(2, false);
        assert_eq!(w.add_to_aoi(2, false), AddOutcome::AlreadyPresent);

        assert_eq!(w.events(), &[AoIEvent::Entered(2)], "no second enter");
        assert_eq!(w.len(), 1, "no duplicate cache");
        assert!(!w.cache(2).unwrap().is_gone());
    }

    /// A manually added entity is held by both sources; clearing one source
    /// keeps it in the AoI (`witness.cpp:2305`).
    #[test]
    fn a_manual_and_trigger_add_are_both_held() {
        let mut w = witness();
        w.add_to_aoi(2, false); // trigger
        w.add_to_aoi(2, true); // manual

        assert_eq!(w.remove_from_aoi(2, false), RemoveOutcome::StillHeld);
        assert!(w.inside(2), "still held manually");
        assert_eq!(w.remove_from_aoi(2, true), RemoveOutcome::Removed);
        assert!(w.cache(2).unwrap().is_gone());
        assert!(!w.inside(2));
    }

    /// `setAoIRadius` clamps the radius to `0.1` and to the maximum.
    #[test]
    fn set_radius_clamps_low_and_high() {
        let mut w = witness();
        w.set_radius(0.0);
        assert_eq!(w.radius(), 0.1);
        w.set_radius(10_000.0);
        assert_eq!(w.radius(), 500.0, "clamped to max_radius");
        w.set_radius(50.0);
        assert_eq!(w.radius(), 50.0);
    }

    /// `addToSeen` seeds a new cache with the current minimum priority, so it
    /// waits its turn rather than jumping the queue.
    #[test]
    fn a_new_entity_starts_at_the_minimum_priority() {
        let mut w = witness();
        w.add_to_aoi(2, false);
        assert_eq!(w.cache(2).unwrap().priority(), 0.0);
        w.add_to_aoi(3, false);
        assert_eq!(w.cache(3).unwrap().priority(), 0.0);

        // Raise entity 2 above entity 3.
        w.cache_mut(2).unwrap().set_priority(7.0);

        w.add_to_aoi(4, false);
        assert_eq!(w.cache(4).unwrap().priority(), 0.0, "seeded from the min");
        assert_eq!(w.entity_queue()[0], 3, "front is the minimum priority");
        assert_eq!(w.entity_queue().len(), 3);
    }

    /// Id aliases are handed out from the pool and recycled on leave.
    #[test]
    fn id_aliases_are_allocated_and_recycled() {
        let mut w = witness();
        let alias = w.allocate_id_alias();
        assert_ne!(alias, NO_ID_ALIAS);

        w.recycle_id_alias(alias);
        assert_eq!(w.allocate_id_alias(), alias, "recycled alias is reused");
    }

    /// A destroyed entity's cache is dropped from the map and the queue.
    #[test]
    fn deleting_a_cache_removes_it_from_the_map_and_queue() {
        let mut w = witness();
        w.add_to_aoi(2, false);
        w.add_to_aoi(3, false);
        assert_eq!(w.len(), 2);

        w.delete_cache(2);

        assert!(w.cache(2).is_none());
        assert_eq!(w.len(), 1);
        assert!(!w.entity_queue().contains(&2));
    }

    /// A fixed load of updatable entities at fixed distances, with a budget
    /// for all of them.
    fn fixed_load() -> (Witness, BTreeMap<EntityId, Position>) {
        let mut w = Witness::new(1, 50.0, 500.0);
        let positions = BTreeMap::from([
            (2, Position::new(10.0, 0.0)),
            (3, Position::new(20.0, 0.0)),
            (4, Position::new(30.0, 0.0)),
        ]);
        for &entity in &[2u32, 3, 4] {
            w.add_to_aoi(entity, false);
            // The enter was already sent: the entity is updatable.
            w.cache_mut(entity).unwrap().clear_enter_pending();
        }
        (w, positions)
    }

    /// The acceptance: under a fixed load the update sends the entities in
    /// priority order — closest first — and accumulates a distance-based
    /// priority on each one sent.
    #[test]
    fn updates_are_selected_closest_first_and_priorities_accumulate() {
        let (mut w, positions) = fixed_load();
        let scheme = AoIUpdateScheme::default_scheme(500.0);

        let result = w.update(Position::ZERO, 2 * UPDATE_BYTES, &scheme, &positions);

        assert_eq!(result.sent_updates, vec![2, 3], "closest first");
        assert!(result.sent_enters.is_empty());
        assert!(result.deleted.is_empty());
        assert_eq!(result.bandwidth_deficit, 0);
        // Priorities diverge by distance: closer = smaller delta = lower
        // priority, so they stay near the front.
        let p2 = w.cache(2).unwrap().priority();
        let p3 = w.cache(3).unwrap().priority();
        let p4 = w.cache(4).unwrap().priority();
        assert!(p2 < p3, "closer entities have lower priority");
        assert!(p2 > 0.0, "priorities accumulated");
        assert_eq!(p4, 0.0, "the unsent entity stays at zero");
    }

    /// The budget bounds the sequence: only the closest fit.
    #[test]
    fn the_budget_bounds_the_send_sequence() {
        let (mut w, positions) = fixed_load();
        let scheme = AoIUpdateScheme::default_scheme(500.0);

        let result = w.update(Position::ZERO, UPDATE_BYTES, &scheme, &positions);

        assert_eq!(result.sent_updates, vec![2], "only the closest");
        assert_eq!(result.bandwidth_deficit, 0);
    }

    /// When the whole queue is sent, every priority resets to 0 so the next
    /// tick starts from an even order (`witness.cpp:1300`).
    #[test]
    fn sending_everything_resets_all_priorities() {
        let (mut w, positions) = fixed_load();
        let scheme = AoIUpdateScheme::default_scheme(500.0);

        // A full-budget update sends everything and resets all priorities.
        let result = w.update(Position::ZERO, 3 * UPDATE_BYTES, &scheme, &positions);

        assert_eq!(result.sent_updates, vec![2, 3, 4]);
        assert_eq!(w.cache(2).unwrap().priority(), 0.0);
        assert_eq!(w.cache(3).unwrap().priority(), 0.0);
        assert_eq!(w.cache(4).unwrap().priority(), 0.0);
    }

    /// A far entity whose priority has grown past `maxPriority` is capped out
    /// (`witness.cpp:1135`): it is not sent every tick, even when the budget
    /// still has room.
    #[test]
    fn a_far_entity_is_capped_out_by_max_priority() {
        let mut w = Witness::new(1, 50.0, 500.0);
        let positions = BTreeMap::from([
            (2, Position::new(10.0, 0.0)),
            (3, Position::new(400.0, 0.0)),
            (4, Position::new(20.0, 0.0)),
        ]);
        for &entity in &[2u32, 3, 4] {
            w.add_to_aoi(entity, false);
            w.cache_mut(entity).unwrap().clear_enter_pending();
        }
        let scheme = AoIUpdateScheme::default_scheme(500.0);

        // A partial update: the near entities get small deltas, the far one a
        // large one.
        let result = w.update(Position::ZERO, 2 * UPDATE_BYTES, &scheme, &positions);
        assert_eq!(result.sent_updates, vec![2, 3]);
        let far_priority = w.cache(3).unwrap().priority();
        assert!(far_priority > w.cache(2).unwrap().priority());

        // Now the far entity exceeds front + MAX_PRIORITY_DELTA and is capped
        // out even with an ample budget.
        let result = w.update(Position::ZERO, 3 * UPDATE_BYTES, &scheme, &positions);
        assert_eq!(
            result.sent_updates,
            vec![4, 2],
            "the far entity is capped out"
        );
        assert_eq!(
            w.cache(3).unwrap().priority(),
            far_priority,
            "the far entity was not updated"
        );
    }
}
