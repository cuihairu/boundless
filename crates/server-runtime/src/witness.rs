//! BW-08-03-02: the witness and its entity caches.
//!
//! Ported from the reference faces:
//! `server/cellapp/entity_cache.{hpp,cpp,ipp}` (the cached state per seen
//! entity and its client-lifecycle flags), and the witness management faces
//! in `server/cellapp/witness.cpp`: `AoITrigger::triggerEnter/triggerLeave`
//! (`witness.cpp:3586-3614`), `Witness::addToAoI` / `removeFromAoI`
//! (the manual/trigger two-holder bookkeeping), `Witness::addToSeen` /
//! `deleteFromAoI` (the known-entity queue) and the `Witness::~Witness`
//! teardown (unlink the AoI trigger without contraction, drop the caches).
//!
//! The reference reads type facts (`canBeOnClient`, `isManualAoI`,
//! destroyed, event numbers) off the live `Entity`; here they arrive via
//! [`EntityFacts`] registered per entity, which keeps this module free of the
//! entitydef layer. Enter/leave range events are drained from
//! [`RangeList::drain_events_for`] and applied exactly as the reference's
//! trigger callbacks would.

use crate::entity_creator::EntityId;
use crate::range_list::{EventKind, NodeId, RangeList, TriggerId};
use foundation::math::Vector3;
use std::collections::{BTreeMap, HashMap};

/// `NO_ID_ALIAS` (`entity_cache.hpp:27`).
pub const NO_ID_ALIAS: u8 = 0xff;

/// `EntityCache::MAX_LOD_LEVELS` (`entity_cache.hpp:37`).
pub const MAX_LOD_LEVELS: usize = 4;

/// `EntityCache::Priority`.
pub type Priority = f64;

/// The EntityCache flag bits (`entity_cache.hpp:138-162`). A cache is
/// updatable only while none of [`cache_flags::NOT_UPDATABLE`] is set.
pub mod cache_flags {
    /// Waiting to send enterAoI to client.
    pub const ENTER_PENDING: u16 = 1 << 0;
    /// Expecting requestEntityUpdate from client.
    pub const REQUEST_PENDING: u16 = 1 << 1;
    /// Waiting to send createEntity to client.
    pub const CREATE_PENDING: u16 = 1 << 2;
    /// Waiting to remove from priority queue.
    pub const GONE: u16 = 1 << 3;
    /// Do not send to client.
    pub const WITHHELD: u16 = 1 << 4;
    /// Waiting to be removed and re-added to the AoI.
    pub const REFRESH: u16 = 1 << 5;
    /// Should be sent in the next `Witness::update()`.
    pub const PRIORITISED: u16 = 1 << 6;
    /// Added manually to the AoI.
    pub const MANUALLY_ADDED: u16 = 1 << 7;
    /// Added automatically by the trigger.
    pub const ADDED_BY_TRIGGER: u16 = 1 << 8;
    /// Volatile position updates are detailed at all distances.
    pub const IS_ALWAYS_DETAILED: u16 = 1 << 9;
    /// Any of these set means no normal update in
    /// `RealEntityWithWitnesses::update`.
    pub const NOT_UPDATABLE: u16 =
        ENTER_PENDING | REQUEST_PENDING | CREATE_PENDING | GONE | WITHHELD | REFRESH;
    /// Client related state, reset when the entity leaves the client.
    pub const CLIENT_STATE: u16 = ENTER_PENDING | REQUEST_PENDING | CREATE_PENDING | GONE | REFRESH;
}

/// The per-entity facts the reference reads off the live `Entity` and its
/// type description: `canBeOnClient` / `isManualAoI` from the description,
/// `isDestroyed`, `lastEventNumber`, `volatileUpdateNumber`, the number of
/// LoD levels and the event-history floor (`lastTrimmedEventNumber`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EntityFacts {
    /// `canBeOnClient`.
    pub client_visible: bool,
    /// `isManualAoI`.
    pub manual_aoi: bool,
    /// `isDestroyed`.
    pub destroyed: bool,
    /// `lastEventNumber`.
    pub last_event_number: i32,
    /// `volatileUpdateNumber`.
    pub volatile_update_number: u16,
    /// `numLoDLevels` (max [`MAX_LOD_LEVELS`]).
    pub detail_level: u8,
    /// `lastTrimmedEventNumber`.
    pub history_floor: i32,
}

impl Default for EntityFacts {
    fn default() -> Self {
        EntityFacts {
            client_visible: true,
            manual_aoi: false,
            destroyed: false,
            last_event_number: 0,
            volatile_update_number: 0,
            detail_level: 1,
            history_floor: 0,
        }
    }
}

/// Cached state about one seen entity (`entity_cache.hpp:33-195`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EntityCache {
    /// The seen entity's id (`pEntity_`).
    pub entity_id: EntityId,
    flags: u16,
    /// `priority_`.
    priority: Priority,
    /// `lastPriorityDelta_`.
    last_priority_delta: Priority,
    /// `lastEventNumber_`.
    last_event_number: i32,
    /// `lastVolatileUpdateNumber_`.
    last_volatile_update_number: u16,
    /// `detailLevel_`.
    detail_level: u8,
    /// `idAlias_`.
    id_alias: u8,
    /// `lodEventNumbers_`.
    lod_event_numbers: [i32; MAX_LOD_LEVELS],
}

impl EntityCache {
    /// The constructor plus `resetClientState`
    /// (`entity_cache.ipp:24-60`): a fresh cache starts with the entity's
    /// current event number, one below its volatile number, at its outer
    /// detail level, unaliased, with an uncapped priority delta.
    fn new(entity_id: EntityId, facts: &EntityFacts) -> Self {
        EntityCache {
            entity_id,
            flags: 0,
            priority: 0.0,
            last_priority_delta: f64::MAX,
            last_event_number: facts.last_event_number,
            last_volatile_update_number: facts.volatile_update_number.wrapping_sub(1),
            detail_level: facts.detail_level,
            id_alias: NO_ID_ALIAS,
            lod_event_numbers: [0; MAX_LOD_LEVELS],
        }
    }

    /// `resetClientState` (`entity_cache.ipp:40-60`): the client state is
    /// re-seeded from the entity.
    fn reset_client_state(&mut self, facts: &EntityFacts) {
        self.last_event_number = facts.last_event_number;
        self.last_volatile_update_number = facts.volatile_update_number.wrapping_sub(1);
        self.detail_level = facts.detail_level;
        self.id_alias = NO_ID_ALIAS;
        self.last_priority_delta = f64::MAX;
        self.flags &= !cache_flags::CLIENT_STATE;
        self.lod_event_numbers = [0; MAX_LOD_LEVELS];
    }

    /// `onEntityRemovedFromClient` (`entity_cache.ipp:64-70`).
    pub fn on_entity_removed_from_client(&mut self, facts: &EntityFacts) {
        self.reset_client_state(facts);
    }

    /// `reuse` (`entity_cache.cpp:382-403`): a gone cache is revived; if the
    /// client has missed events past the history floor, mark it for a full
    /// out-and-back-in refresh instead.
    fn reuse(&mut self, facts: &EntityFacts) {
        debug_assert!(self.is_gone());
        self.clear_gone();
        if self.last_event_number < facts.history_floor && self.is_updatable() {
            self.set_refresh();
        }
    }

    /// Sets the flag bits in `bits`.
    fn set_flags(&mut self, bits: u16) {
        self.flags |= bits;
    }

    /// Clears the flag bits in `bits`.
    fn clear_flags(&mut self, bits: u16) {
        self.flags &= !bits;
    }

    /// Whether all of `bits` are set.
    fn has_flags(&self, bits: u16) -> bool {
        (self.flags & bits) == bits
    }

    /// `setEnterPending` / `clearEnterPending` / `isEnterPending`.
    pub fn set_enter_pending(&mut self) {
        self.set_flags(cache_flags::ENTER_PENDING);
    }
    pub fn clear_enter_pending(&mut self) {
        self.clear_flags(cache_flags::ENTER_PENDING);
    }
    pub fn is_enter_pending(&self) -> bool {
        self.has_flags(cache_flags::ENTER_PENDING)
    }

    /// `setRequestPending` / `clearRequestPending` / `isRequestPending`.
    pub fn set_request_pending(&mut self) {
        self.set_flags(cache_flags::REQUEST_PENDING);
    }
    pub fn clear_request_pending(&mut self) {
        self.clear_flags(cache_flags::REQUEST_PENDING);
    }
    pub fn is_request_pending(&self) -> bool {
        self.has_flags(cache_flags::REQUEST_PENDING)
    }

    /// `setCreatePending` / `clearCreatePending` / `isCreatePending`.
    pub fn set_create_pending(&mut self) {
        self.set_flags(cache_flags::CREATE_PENDING);
    }
    pub fn clear_create_pending(&mut self) {
        self.clear_flags(cache_flags::CREATE_PENDING);
    }
    pub fn is_create_pending(&self) -> bool {
        self.has_flags(cache_flags::CREATE_PENDING)
    }

    /// `setGone` / `clearGone` / `isGone`.
    pub fn set_gone(&mut self) {
        self.set_flags(cache_flags::GONE);
    }
    pub fn clear_gone(&mut self) {
        self.clear_flags(cache_flags::GONE);
    }
    pub fn is_gone(&self) -> bool {
        self.has_flags(cache_flags::GONE)
    }

    /// `setWithheld` / `isWithheld`.
    pub fn set_withheld(&mut self) {
        self.set_flags(cache_flags::WITHHELD);
    }
    pub fn is_withheld(&self) -> bool {
        self.has_flags(cache_flags::WITHHELD)
    }

    /// `setRefresh` / `clearRefresh` / `isRefresh`.
    pub fn set_refresh(&mut self) {
        self.set_flags(cache_flags::REFRESH);
    }
    pub fn clear_refresh(&mut self) {
        self.clear_flags(cache_flags::REFRESH);
    }
    pub fn is_refresh(&self) -> bool {
        self.has_flags(cache_flags::REFRESH)
    }

    /// `setPrioritised` / `clearPrioritised` / `isPrioritised`.
    pub fn set_prioritised(&mut self) {
        self.set_flags(cache_flags::PRIORITISED);
    }
    pub fn clear_prioritised(&mut self) {
        self.clear_flags(cache_flags::PRIORITISED);
    }
    pub fn is_prioritised(&self) -> bool {
        self.has_flags(cache_flags::PRIORITISED)
    }

    /// `setManuallyAdded` / `isManuallyAdded`.
    pub fn set_manually_added(&mut self) {
        self.set_flags(cache_flags::MANUALLY_ADDED);
    }
    pub fn is_manually_added(&self) -> bool {
        self.has_flags(cache_flags::MANUALLY_ADDED)
    }

    /// `setAddedByTrigger` / `isAddedByTrigger`.
    pub fn set_added_by_trigger(&mut self) {
        self.set_flags(cache_flags::ADDED_BY_TRIGGER);
    }
    pub fn is_added_by_trigger(&self) -> bool {
        self.has_flags(cache_flags::ADDED_BY_TRIGGER)
    }

    /// `isUpdatable` (`entity_cache.hpp:131`).
    pub fn is_updatable(&self) -> bool {
        (self.flags & cache_flags::NOT_UPDATABLE) == 0
    }

    /// `priority()`.
    pub fn priority(&self) -> Priority {
        self.priority
    }

    /// `priority(newPriority)`.
    pub fn set_priority(&mut self, value: Priority) {
        self.priority = value;
    }

    /// `lastPriorityDelta()`.
    pub fn last_priority_delta(&self) -> Priority {
        self.last_priority_delta
    }

    /// `lastPriorityDelta(newDelta)`.
    pub fn set_last_priority_delta(&mut self, value: Priority) {
        self.last_priority_delta = value;
    }

    /// `lastEventNumber()`.
    pub fn last_event_number(&self) -> i32 {
        self.last_event_number
    }

    /// `lastVolatileUpdateNumber()`.
    pub fn last_volatile_update_number(&self) -> u16 {
        self.last_volatile_update_number
    }

    /// `detailLevel()`.
    pub fn detail_level(&self) -> u8 {
        self.detail_level
    }

    /// `idAlias()`.
    pub fn id_alias(&self) -> u8 {
        self.id_alias
    }
}

/// A witness: a real entity's view of the entities around it, driven by an
/// AoI range trigger and the cache map + known-entity queue
/// (`witness.hpp:48-`, `aoiMap_` / `entityQueue_`).
pub struct Witness {
    /// The witness entity's range-list node.
    pub node: NodeId,
    /// The AoI trigger id in the shared range list.
    pub aoi: TriggerId,
    /// The witness entity's id.
    pub entity_id: EntityId,
    /// `aoiMap_`: one cache per seen entity, keyed by id (`EntityCacheMap`).
    caches: BTreeMap<EntityId, EntityCache>,
    /// `entityQueue_`: the known entities, in add order
    /// (`witness.hpp:201`).
    queue: Vec<EntityId>,
    /// Per-entity facts, registered or defaulted.
    facts: HashMap<EntityId, EntityFacts>,
}

impl Witness {
    /// Witness creation around `entity_id` at `pos` with an AoI `radius`:
    /// the AoI trigger inserts (firing enter events for everything already
    /// in radius) and the large entities whose appeal range covers the
    /// position join immediately (`AoITrigger` constructor,
    /// `witness.cpp:73-101`, including `visitLargeEntities`).
    pub fn new(list: &mut RangeList, entity_id: EntityId, pos: Vector3, radius: f32) -> Self {
        let node = list.add_entity(entity_id, pos);
        let aoi = list.add_aoi_trigger(node, radius);
        let mut witness = Witness {
            node,
            aoi,
            entity_id,
            caches: BTreeMap::new(),
            queue: Vec::new(),
            facts: HashMap::new(),
        };
        // visitLargeEntities: appeal-covered entities join before the
        // trigger's own insert events are applied.
        for large in list.appeal_entities_containing(pos.x, pos.z) {
            if large != entity_id {
                witness.add_to_aoi(large, false);
            }
        }
        list.trigger_insert(aoi);
        witness.poll(list);
        witness
    }

    /// Register the facts for an entity (the type-description side of the
    /// reference). Unregistered entities use [`EntityFacts::default`].
    pub fn register(&mut self, entity_id: EntityId, facts: EntityFacts) {
        self.facts.insert(entity_id, facts);
    }

    fn facts_of(&self, entity_id: EntityId) -> EntityFacts {
        *self
            .facts
            .get(&entity_id)
            .unwrap_or(&EntityFacts::default())
    }

    /// Apply this witness's share of the range list's enter/leave events,
    /// exactly as its `AoITrigger::triggerEnter` / `triggerLeave` callbacks
    /// would (`witness.cpp:3586-3614`): other triggers' events stay queued
    /// for them, and the witness's own entity plus manual-AoI types are
    /// skipped.
    pub fn poll(&mut self, list: &mut RangeList) {
        for event in list.drain_events_for(self.aoi) {
            if event.entity == self.entity_id {
                continue;
            }
            if self.facts_of(event.entity).manual_aoi {
                continue;
            }
            match event.kind {
                EventKind::Enter => self.add_to_aoi(event.entity, false),
                EventKind::Leave => self.remove_from_aoi(event.entity, false),
            }
        }
    }

    /// `Witness::addToAoI` (`witness.cpp`, the management half): destroyed
    /// and non-client entities never join; a present-and-live cache just
    /// gains the holder flag; a gone cache is reused; a fresh entity gets a
    /// cache, enter-pending and a queue slot.
    pub fn add_to_aoi(&mut self, entity_id: EntityId, set_manually_added: bool) {
        let facts = self.facts_of(entity_id);
        if facts.destroyed || !facts.client_visible {
            return;
        }
        if let Some(cache) = self.caches.get_mut(&entity_id) {
            // Present: a gone cache is revived, a live one is a no-op apart
            // from the holder flag.
            if cache.is_gone() {
                cache.reuse(&facts);
            } else {
                if set_manually_added {
                    cache.set_manually_added();
                } else {
                    cache.set_added_by_trigger();
                }
                return;
            }
        } else {
            let mut cache = EntityCache::new(entity_id, &facts);
            cache.set_enter_pending();
            self.add_to_seen(entity_id, &mut cache);
            self.caches.insert(entity_id, cache);
        }
        let cache = self.caches.get_mut(&entity_id).unwrap();
        if set_manually_added {
            cache.set_manually_added();
        } else {
            cache.set_added_by_trigger();
        }
    }

    /// `Witness::removeFromAoI`: with `clear_manually_added` the manual hold
    /// is dropped first (a trigger hold keeps the entity), otherwise the
    /// trigger hold is dropped first. The last holder out marks the cache
    /// gone; unknown entities are ignored.
    pub fn remove_from_aoi(&mut self, entity_id: EntityId, clear_manually_added: bool) {
        let facts = self.facts_of(entity_id);
        if !facts.client_visible {
            return;
        }
        let Some(cache) = self.caches.get_mut(&entity_id) else {
            return;
        };
        if clear_manually_added {
            cache.clear_flags(cache_flags::MANUALLY_ADDED);
            if cache.is_added_by_trigger() {
                return;
            }
        } else {
            cache.clear_flags(cache_flags::ADDED_BY_TRIGGER);
            if cache.is_manually_added() {
                return;
            }
        }
        cache.set_gone();
    }

    /// `Witness::addToSeen` (`witness.cpp`): the newcomer starts at the
    /// current front priority so it does not jump the queue, then joins the
    /// back.
    fn add_to_seen(&mut self, entity_id: EntityId, cache: &mut EntityCache) {
        let initial_priority = self
            .queue
            .first()
            .and_then(|front| self.caches.get(front))
            .map(|front| front.priority())
            .unwrap_or(0.0);
        cache.set_priority(initial_priority);
        self.queue.push(entity_id);
    }

    /// `Witness::deleteFromAoI` (`witness.cpp`): swap-remove the queue slot
    /// and drop the cache.
    pub fn delete_from_aoi(&mut self, entity_id: EntityId) {
        if let Some(pos) = self.queue.iter().position(|id| *id == entity_id) {
            let last = self.queue.len() - 1;
            self.queue.swap(pos, last);
            self.queue.pop();
        }
        self.caches.remove(&entity_id);
    }

    /// `Witness::~Witness` (the AoI half): unlink the AoI trigger without
    /// contraction (no per-entity leaves — the AoI goes away wholesale) and
    /// drop the caches.
    pub fn destroy(&mut self, list: &mut RangeList) {
        list.trigger_remove_without_contracting(self.aoi);
        self.caches.clear();
        self.queue.clear();
    }

    /// The cache for a seen entity, if any.
    pub fn cache(&self, entity_id: EntityId) -> Option<&EntityCache> {
        self.caches.get(&entity_id)
    }

    /// Mutable cache access for the update/selection paths
    /// (`aoiMap_` mutation faces).
    pub fn cache_mut(&mut self, entity_id: EntityId) -> Option<&mut EntityCache> {
        self.caches.get_mut(&entity_id)
    }

    /// `Witness::isInAoI`: cached and not gone.
    pub fn is_in_aoi(&self, entity_id: EntityId) -> bool {
        matches!(self.caches.get(&entity_id), Some(c) if !c.is_gone())
    }

    /// The ids with a cache, gone ones included, in id order
    /// (`EntityCacheMap` iteration order).
    pub fn aoi_ids(&self) -> Vec<EntityId> {
        self.caches.keys().copied().collect()
    }

    /// The known-entity queue, front first (`entityQueue_`).
    pub fn queue(&self) -> &[EntityId] {
        &self.queue
    }

    /// The number of caches (`EntityCacheMap::size`).
    pub fn cache_count(&self) -> usize {
        self.caches.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::range_list::RangeList;

    fn v(x: f32, z: f32) -> Vector3 {
        Vector3::new(x, 0.0, z)
    }

    /// A witness (entity 1) at the origin with a 10-unit AoI.
    fn setup() -> (RangeList, Witness) {
        let mut list = RangeList::new();
        let w = Witness::new(&mut list, 1, v(0.0, 0.0), 10.0);
        (list, w)
    }

    /// The observable snapshot of a cache: every flag, the queue-visible
    /// numbers and the bookkeeping state.
    fn snapshot(c: &EntityCache) -> String {
        format!(
            "flags={:04x} prio={} evn={} lod{} alias={} vol={} delta={}",
            c.flags,
            c.priority(),
            c.last_event_number(),
            c.detail_level(),
            c.id_alias(),
            c.last_volatile_update_number(),
            c.last_priority_delta(),
        )
    }

    #[test]
    fn first_observation_and_reentry_produce_same_result() {
        let (mut list, mut w) = setup();
        let b = list.add_entity(2, v(5.0, 5.0));
        w.poll(&mut list);

        // First observation: cached, enter-pending, queued once.
        assert!(w.is_in_aoi(2));
        let first = w.cache(2).copied().unwrap();
        assert!(first.is_enter_pending());
        assert!(first.is_added_by_trigger());
        assert!(!first.is_manually_added());
        assert!(!first.is_gone());
        assert_eq!(w.queue(), &[2]);

        // Walk out: cache marked gone but the slot survives.
        list.move_entity(b, v(50.0, 50.0));
        w.poll(&mut list);
        assert!(!w.is_in_aoi(2));
        assert!(w.cache(2).unwrap().is_gone());
        assert_eq!(w.cache_count(), 1);

        // Walk back in: the same cache slot is reused and the observable
        // result matches the first observation exactly.
        list.move_entity(b, v(5.0, 5.0));
        w.poll(&mut list);
        assert!(w.is_in_aoi(2));
        let again = w.cache(2).copied().unwrap();
        assert_eq!(snapshot(&again), snapshot(&first));
        assert_eq!(w.queue(), &[2], "no duplicate queue slot on re-entry");
        assert_eq!(w.cache_count(), 1);
    }

    #[test]
    fn double_add_is_idempotent() {
        let (_list, mut w) = setup();
        w.add_to_aoi(7, false);
        w.add_to_aoi(7, false);
        assert_eq!(w.cache_count(), 1);
        assert_eq!(w.queue(), &[7]);
        let c = w.cache(7).unwrap();
        assert!(c.is_enter_pending());
        assert!(c.is_added_by_trigger());
    }

    #[test]
    fn manual_and_trigger_holders() {
        let (mut list, mut w) = setup();
        w.add_to_aoi(3, true);
        let e = list.add_entity(3, v(4.0, 0.0));
        w.poll(&mut list);
        let c = w.cache(3).unwrap();
        assert!(c.is_manually_added());
        assert!(c.is_added_by_trigger());

        // Re-adding manually while live is a pure flag OR.
        w.add_to_aoi(3, true);
        assert!(w.cache(3).unwrap().is_manually_added());

        // Clearing the manual hold while the trigger still holds it is a
        // no-op on the live cache.
        w.remove_from_aoi(3, true);
        let c = w.cache(3).unwrap();
        assert!(!c.is_manually_added());
        assert!(c.is_added_by_trigger());
        assert!(!c.is_gone());

        // The trigger leave drops only the trigger hold.
        list.move_entity(e, v(40.0, 0.0));
        w.poll(&mut list);
        let c = w.cache(3).unwrap();
        assert!(!c.is_manually_added());
        assert!(!c.is_added_by_trigger());
        assert!(c.is_gone());
        assert!(!w.is_in_aoi(3));
    }

    #[test]
    fn non_client_visible_entities_are_ignored() {
        let (mut list, mut w) = setup();
        w.register(
            4,
            EntityFacts {
                client_visible: false,
                ..EntityFacts::default()
            },
        );
        let e = list.add_entity(4, v(1.0, 0.0));
        w.poll(&mut list);
        assert!(w.cache(4).is_none());
        w.add_to_aoi(4, true);
        assert!(w.cache(4).is_none());
        w.remove_from_aoi(4, false);
        w.remove_from_aoi(4, true);
        assert_eq!(w.cache_count(), 0);
        list.remove_entity(e);
        list.drain_events();
    }

    #[test]
    fn manual_aoi_types_skip_the_trigger_but_allow_explicit_add() {
        let (mut list, mut w) = setup();
        w.register(
            5,
            EntityFacts {
                manual_aoi: true,
                ..EntityFacts::default()
            },
        );
        let e = list.add_entity(5, v(2.0, 0.0));
        w.poll(&mut list);
        assert!(w.cache(5).is_none());
        w.add_to_aoi(5, false);
        assert!(w.is_in_aoi(5));
        list.remove_entity(e);
        list.drain_events();
    }

    #[test]
    fn destroyed_entities_are_ignored() {
        let (_list, mut w) = setup();
        w.register(
            6,
            EntityFacts {
                destroyed: true,
                ..EntityFacts::default()
            },
        );
        w.add_to_aoi(6, false);
        assert!(w.cache(6).is_none());
    }

    #[test]
    fn reuse_after_trim_sets_refresh() {
        let (mut list, mut w) = setup();
        let e = list.add_entity(8, v(3.0, 0.0));
        w.poll(&mut list);
        // Simulate the client having taken the entity, then the event
        // history being trimmed past the cache's last event.
        w.cache_mut(8).unwrap().clear_enter_pending();
        assert!(w.cache(8).unwrap().is_updatable());

        list.move_entity(e, v(80.0, 0.0));
        w.poll(&mut list);
        assert!(w.cache(8).unwrap().is_gone());

        w.register(
            8,
            EntityFacts {
                history_floor: 10,
                ..EntityFacts::default()
            },
        );
        list.move_entity(e, v(3.0, 0.0));
        w.poll(&mut list);
        let c = w.cache(8).unwrap();
        assert!(
            c.is_refresh(),
            "missed events past the floor force a refresh"
        );
        assert!(!c.is_gone());
    }

    #[test]
    fn creation_seeds_aoi_from_insert_and_appeal() {
        let mut list = RangeList::new();
        // Entity 2 sits inside the upcoming AoI; entity 3 is far outside it
        // but its 100-unit appeal range covers the origin.
        let _near = list.add_entity(2, v(5.0, 0.0));
        let large = list.add_entity(3, v(60.0, 0.0));
        let appeal = list.add_appeal_trigger(large, 100.0);
        list.trigger_insert(appeal);
        list.drain_events();

        let w = Witness::new(&mut list, 1, v(0.0, 0.0), 10.0);
        assert!(w.is_in_aoi(2), "insert-time enter lands in the cache");
        assert!(
            w.is_in_aoi(3),
            "appeal-covered large entity joins at creation"
        );
        assert_eq!(
            w.queue(),
            &[3, 2],
            "appeal seeds queue before insert enters"
        );

        // The large entity's own witness skips its own appeal coverage.
        let w3 = Witness::new(&mut list, 3, v(60.0, 0.0), 10.0);
        assert!(w3.cache(3).is_none());
    }

    #[test]
    fn removing_unknown_entities_is_a_noop() {
        let (mut list, mut w) = setup();
        w.remove_from_aoi(99, false);
        w.remove_from_aoi(99, true);
        w.delete_from_aoi(99);
        assert_eq!(w.cache_count(), 0);
        assert!(w.queue().is_empty());
        list.drain_events();
    }

    #[test]
    fn destroy_unlinks_aoi_and_clears_caches() {
        let (mut list, mut w) = setup();
        let _e = list.add_entity(2, v(5.0, 0.0));
        w.poll(&mut list);
        assert_eq!(w.cache_count(), 1);
        assert_eq!(list.trigger_count(), 1);
        // Two entities plus the AoI trigger's two bounds are on the axes.
        assert_eq!(list.iter_x().len(), 4);

        w.destroy(&mut list);
        assert_eq!(w.cache_count(), 0);
        assert!(w.queue().is_empty());
        assert_eq!(list.iter_x().len(), 2, "the AoI bounds leave both axes");
        assert_eq!(list.iter_z().len(), 2);
        assert!(list.drain_events().is_empty(), "no per-entity leaves fire");
        w.poll(&mut list);
        assert_eq!(w.cache_count(), 0);
    }

    #[test]
    fn delete_from_aoi_removes_queue_slot_and_cache() {
        let (mut list, mut w) = setup();
        let _a = list.add_entity(2, v(5.0, 0.0));
        let _b = list.add_entity(3, v(6.0, 0.0));
        w.poll(&mut list);
        assert_eq!(w.queue(), &[2, 3]);
        assert_eq!(w.aoi_ids(), vec![2, 3]);

        w.delete_from_aoi(2);
        assert_eq!(w.queue(), &[3]);
        assert_eq!(w.aoi_ids(), vec![3]);
        assert!(w.cache(2).is_none());
        assert!(w.cache(3).is_some());
    }

    #[test]
    fn poll_skips_other_triggers_and_the_witnesss_own_events() {
        let (mut list, mut w) = setup();
        // A second witness sits just outside the first one's AoI.
        let mut w2 = Witness::new(&mut list, 2, v(15.0, 0.0), 10.0);

        // Walking witness 1 toward witness 2 drags its upper bound past
        // entity 2, which genuinely enters the widened AoI. The crossing
        // with witness 2's bounds belongs to witness 2's trigger and must
        // not leak into witness 1's caches.
        list.move_entity(w.node, v(12.0, 0.0));
        w.poll(&mut list);
        assert!(w.is_in_aoi(2));
        assert!(w.cache(1).is_none());
        assert_eq!(w.cache_count(), 1);

        w2.poll(&mut list);
        assert!(w2.is_in_aoi(1));

        // Removing the witness entity fires leaves naming itself (the
        // teleport shuffle); they must not self-seed or self-drop caches.
        list.remove_entity(w.node);
        w.poll(&mut list);
        assert!(w.cache(1).is_none());
        assert!(w.is_in_aoi(2));
        w2.poll(&mut list);
        assert!(!w2.is_in_aoi(1));
    }

    #[test]
    fn newcomers_start_at_the_queue_front_priority() {
        let (mut list, mut w) = setup();
        let _a = list.add_entity(2, v(5.0, 0.0));
        w.poll(&mut list);
        assert_eq!(w.cache(2).unwrap().priority(), 0.0);

        w.cache_mut(2).unwrap().set_priority(5.0);
        let _b = list.add_entity(3, v(7.0, 0.0));
        w.poll(&mut list);
        assert_eq!(w.cache(3).unwrap().priority(), 5.0);
    }

    #[test]
    fn on_entity_removed_from_client_resets_client_state() {
        let (mut list, mut w) = setup();
        let e = list.add_entity(2, v(5.0, 0.0));
        w.poll(&mut list);
        {
            let c = w.cache_mut(2).unwrap();
            c.clear_enter_pending();
            c.set_request_pending();
            c.set_create_pending();
            assert!(!c.is_updatable());
        }
        let facts = EntityFacts {
            last_event_number: 7,
            volatile_update_number: 9,
            detail_level: 3,
            ..EntityFacts::default()
        };
        w.cache_mut(2)
            .unwrap()
            .on_entity_removed_from_client(&facts);
        let c = w.cache(2).unwrap();
        assert!(c.is_updatable());
        assert!(!c.is_enter_pending());
        assert!(!c.is_request_pending());
        assert!(!c.is_create_pending());
        assert_eq!(c.last_event_number(), 7);
        assert_eq!(c.last_volatile_update_number(), 8);
        assert_eq!(c.detail_level(), 3);
        assert_eq!(c.id_alias(), NO_ID_ALIAS);
        assert_eq!(c.entity_id, 2);
        list.remove_entity(e);
        list.drain_events();
    }

    #[test]
    fn cache_flag_semantics() {
        let mut c = EntityCache::new(5, &EntityFacts::default());
        assert!(c.is_updatable());
        assert_eq!(c.last_volatile_update_number(), u16::MAX);
        assert_eq!(c.id_alias(), NO_ID_ALIAS);
        assert_eq!(c.last_priority_delta(), f64::MAX);

        c.set_prioritised();
        assert!(c.is_prioritised());
        assert!(
            c.is_updatable(),
            "prioritised alone keeps the cache updatable"
        );
        c.clear_prioritised();
        assert!(!c.is_prioritised());

        c.set_withheld();
        assert!(c.is_withheld());
        assert!(!c.is_updatable(), "withheld blocks updates");
        c.set_refresh();
        assert!(c.is_refresh());
        c.clear_refresh();
        assert!(!c.is_refresh());

        c.set_request_pending();
        assert!(c.is_request_pending());
        c.clear_request_pending();
        assert!(!c.is_request_pending());
        c.set_create_pending();
        assert!(c.is_create_pending());
        c.clear_create_pending();
        assert!(!c.is_create_pending());

        c.set_last_priority_delta(1.5);
        assert_eq!(c.last_priority_delta(), 1.5);
        assert!(!c.is_updatable(), "withheld survives refresh toggling");
    }
}
