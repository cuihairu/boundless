//! BW-08-03-04: the per-observer client update — volatile position and
//! property sync.
//!
//! Every observer (witness) carries a reference position derived by rounding
//! its own entity position to whole metres. A watched entity's volatile
//! position is sent relative to that reference, so two observers at different
//! positions receive two different projections of the same entity, and the
//! watched entity itself receives only its own fractional offset. When the
//! relative position cannot be represented in the packed form it falls back to
//! a detailed update. Property changes travel through the event history: every
//! event newer than the observer's `lastEventNumber` that passes its send rule
//! is forwarded, then the observer's marker advances.
//!
//! Reference:
//! - `msgtypes.hpp:423` `calculateReferencePosition` — the reference is the
//!   entity position rounded with `BW_ROUNDF` (`floorf(v + 0.5)`,
//!   `mathdef.hpp:57`), so stationary entities do not jitter as the reference
//!   moves.
//! - `entity.cpp:2754` — non-vehicle entities are projected
//!   `localPosition - basePosition`; vehicle entities are sent absolute.
//! - `entity.cpp:2763-2770` — when any packed component falls outside
//!   `[minLimit, maxLimit)` a detailed volatile update is sent instead.
//! - `msgtypes.ipp:417-432` `maxLimit`/`minLimit` — the packable range is
//!   `[minLimit, maxLimit)`: the lower edge inclusive, the upper exclusive.
//! - `msgtypes.ipp:385-400` `getLimit<3, 8>` — 509.5 for the XZ packing.
//! - `cellapp_config.cpp:120-123` — `packedXZScale = maxAoIRadius /
//!   floorf(min(PackedXZ::maxLimit(1), PackedXYZ::maxLimit(1)))` = 500/509, so
//!   the default packable range spans ±500.49 around the reference.
//! - `entity.cpp:2538` — the volatile update is flagged reliable when any
//!   property event accompanies it.
//! - `history_event.hpp:42-56` `Level::shouldSend` — state-change properties
//!   send when `detailLevel <= level`, message events when
//!   `threshold < priority`.
//! - `entity.cpp:2590` — after the update the observer's `lastEventNumber`
//!   advances to the entity's latest event number.

use crate::movement::Vec3;

/// `DEFAULT_AOI_RADIUS` (`cellapp_config.cpp:21`).
pub const DEFAULT_AOI_RADIUS: f64 = 500.0;

/// `getLimit<3, 8>()` (`msgtypes.ipp:385-400`): the exclusive positive limit
/// of the XZ packing at scale 1.
pub const PACK_LIMIT: f64 = 509.5;

/// `maxPackedRelativePositionValue` (`cellapp_config.cpp:120`):
/// `floorf(min(PackedXZ::maxLimit(1), PackedXYZ::maxLimit(1)))`. Both packings
/// use 3 exponent and 8 mantissa bits for X and Z, so both limits are 509.5.
pub const MAX_PACKED_RELATIVE_POSITION_VALUE: f64 = 509.0;

/// `CellAppConfig::packedXZScale` default (`cellapp_config.cpp:123`).
pub const PACKED_XZ_SCALE: f64 = DEFAULT_AOI_RADIUS / MAX_PACKED_RELATIVE_POSITION_VALUE;

/// The quantisation limits of the packed relative position
/// (`cellapp_config.cpp:120-126`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SyncConfig {
    /// `CellAppConfig::packedXZScale()`.
    pub packed_xz_scale: f64,
}

impl SyncConfig {
    /// The derived default: the AoI radius maps onto the whole packable range.
    pub fn new() -> Self {
        SyncConfig {
            packed_xz_scale: PACKED_XZ_SCALE,
        }
    }

    /// `PackedXZ::maxLimit(scale)` (`msgtypes.ipp:417`).
    pub fn max_limit(&self) -> f64 {
        PACK_LIMIT * self.packed_xz_scale
    }

    /// `PackedXZ::minLimit(scale)` (`msgtypes.ipp:429`).
    pub fn min_limit(&self) -> f64 {
        -self.max_limit()
    }
}

impl Default for SyncConfig {
    fn default() -> Self {
        Self::new()
    }
}

/// `BW_ROUNDF` (`mathdef.hpp:57`): `floorf(value + 0.5f)`.
fn round_half_up(value: f64) -> f64 {
    (value + 0.5).floor()
}

/// The observer's reference position (`msgtypes.hpp:423`): its own entity
/// position rounded to whole metres. Rounding keeps stationary entities still:
/// a reference that moved continuously would drag every packed offset with it.
pub fn reference_position(observer_pos: Vec3) -> Vec3 {
    Vec3::new(
        round_half_up(observer_pos.x),
        round_half_up(observer_pos.y),
        round_half_up(observer_pos.z),
    )
}

/// What one observer receives for an entity's volatile position
/// (`entity.cpp:2740-2840`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PositionUpdate {
    /// `PackedXZ(relativePos.x, relativePos.z, scale)` — the entity is on the
    /// ground, so only X and Z are packed.
    PackedXZ { x: f64, z: f64 },
    /// `PackedXYZ(...)` — the entity is airborne, so Y is packed too.
    PackedXYZ { x: f64, y: f64, z: f64 },
    /// `writeVolatileDetailedDataToBundle` (`entity.cpp:2765-2770`): the
    /// relative position lies outside `[minLimit, maxLimit)` and cannot be
    /// represented in the packed form.
    Detailed,
}

/// The per-observer projection of an entity's position
/// (`entity.cpp:2754-2758`).
///
/// A non-vehicle entity is projected `entity_pos - observer_ref`, so each
/// observer measures the entity from its own reference point. A vehicle
/// entity is sent in vehicle coordinates (`isRelative` is false when
/// `pVehicle_ != NULL`). If any packed component falls outside
/// `[minLimit, maxLimit)` the update falls back to the detailed form
/// (`entity.cpp:2763-2770`).
pub fn project_position(
    entity_pos: Vec3,
    observer_ref: Vec3,
    is_vehicle: bool,
    on_ground: bool,
    config: &SyncConfig,
) -> PositionUpdate {
    let relative = if is_vehicle {
        entity_pos
    } else {
        entity_pos - observer_ref
    };

    let (min_limit, max_limit) = (config.min_limit(), config.max_limit());
    let out_of_range = relative.x < min_limit
        || max_limit <= relative.x
        || relative.z < min_limit
        || max_limit <= relative.z;
    if out_of_range {
        return PositionUpdate::Detailed;
    }

    if on_ground {
        PositionUpdate::PackedXZ {
            x: relative.x,
            z: relative.z,
        }
    } else {
        PositionUpdate::PackedXYZ {
            x: relative.x,
            y: relative.y,
            z: relative.z,
        }
    }
}

/// `HistoryEvent::Level` (`history_event.hpp:36-56`): state-change properties
/// carry a detail level, message events a priority.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum EventLevel {
    /// A state-change property: send when `detailLevel <= level`
    /// (`history_event.hpp:47`).
    Detail { level: i32 },
    /// A message event: send when `threshold < priority`
    /// (`history_event.hpp:48`).
    Priority { priority: f64 },
}

impl EventLevel {
    /// `Level::shouldSend(threshold, detailLevel)` (`history_event.hpp:42-46`).
    pub fn should_send(&self, threshold: f64, detail_level: i32) -> bool {
        match *self {
            EventLevel::Detail { level } => detail_level <= level,
            EventLevel::Priority { priority } => threshold < priority,
        }
    }
}

/// A recorded property change or message (`history_event.hpp:25`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PropertyEvent {
    /// `number_`: the event number in the entity's history.
    pub number: u64,
    /// `msgID_`.
    pub msg_id: u8,
    /// `isReliable_`.
    pub is_reliable: bool,
    /// `level_`: the detail level or priority the event demands.
    pub level: EventLevel,
    /// `latestEventIndex_`: `Some` marks a latest-change-only property
    /// (`history_event.cpp:199-216` collapses the older event of the same
    /// member), `None` a full-history one.
    pub latest_event_index: Option<usize>,
}

/// Selects the property events one observer must receive
/// (`entity.cpp:2520-2586`).
///
/// Every event with `number > last_event_number` that passes
/// `shouldSend(threshold, detail_level)` is returned in ascending
/// event-number order; the caller then advances the observer's
/// `lastEventNumber` to the entity's latest event number
/// (`entity.cpp:2590`), so each event is sent at most once.
pub fn select_events(
    events: &[PropertyEvent],
    last_event_number: u64,
    threshold: f64,
    detail_level: i32,
) -> Vec<PropertyEvent> {
    let mut pending: Vec<PropertyEvent> = events
        .iter()
        .filter(|event| event.number > last_event_number)
        .copied()
        .collect();
    pending.retain(|event| event.level.should_send(threshold, detail_level));
    pending.sort_by_key(|event| event.number);
    pending
}

/// One observer's view of one entity: the inputs that decide what it may
/// receive this tick.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ObserverView {
    /// The observer's own entity position; its rounding is the reference.
    pub observer_pos: Vec3,
    /// `pVehicle_ != NULL`: the watched entity rides a vehicle, so it is
    /// sent absolute (`entity.cpp:2754`).
    pub is_vehicle: bool,
    /// `this->isOnGround()`: on the ground only X and Z are packed.
    pub on_ground: bool,
    /// `cache.lastEventNumber()`.
    pub last_event_number: u64,
    /// `lodPriority`, the threshold property messages must beat.
    pub threshold: f64,
    /// `cache.detailLevel()`; 0 is the most detailed.
    pub detail_level: i32,
}

impl ObserverView {
    /// An observer of a non-vehicle, grounded entity, with an empty history
    /// marker and no filtering.
    pub fn new(observer_pos: Vec3) -> Self {
        ObserverView {
            observer_pos,
            is_vehicle: false,
            on_ground: true,
            last_event_number: 0,
            threshold: f64::NEG_INFINITY,
            detail_level: i32::MAX,
        }
    }

    /// The observer's reference position (`msgtypes.hpp:423`).
    pub fn reference(&self) -> Vec3 {
        reference_position(self.observer_pos)
    }
}

/// What one observer receives for one entity in one update tick
/// (`entity.cpp:2485` `writeClientUpdateDataToBundle` in miniature: volatile
/// position, then the event-history properties).
#[derive(Debug, Clone, PartialEq)]
pub struct ClientUpdate {
    /// The volatile position projection.
    pub position: PositionUpdate,
    /// The selected property events, oldest first.
    pub events: Vec<PropertyEvent>,
    /// `entity.cpp:2538`: `isReliable = hasEventsToSend` — the volatile
    /// update is made reliable when any property event accompanies it.
    pub is_reliable: bool,
}

impl ClientUpdate {
    /// Builds the update one observer receives for one entity.
    pub fn build(
        entity_pos: Vec3,
        entity_events: &[PropertyEvent],
        observer: ObserverView,
        config: &SyncConfig,
    ) -> Self {
        let position = project_position(
            entity_pos,
            observer.reference(),
            observer.is_vehicle,
            observer.on_ground,
            config,
        );
        let events = select_events(
            entity_events,
            observer.last_event_number,
            observer.threshold,
            observer.detail_level,
        );
        let is_reliable = !events.is_empty();
        ClientUpdate {
            position,
            events,
            is_reliable,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(number: u64, level: EventLevel) -> PropertyEvent {
        PropertyEvent {
            number,
            msg_id: 1,
            is_reliable: true,
            level,
            latest_event_index: None,
        }
    }

    #[test]
    fn reference_position_rounds_to_whole_metres() {
        // msgtypes.hpp:423 rounds each axis with BW_ROUNDF (floor(v + 0.5)).
        let pos = Vec3::new(12.4, 0.0, 13.6);
        let reference = reference_position(pos);
        assert_eq!(reference, Vec3::new(12.0, 0.0, 14.0));
    }

    #[test]
    fn negative_half_metres_round_up_not_away_from_zero() {
        // BW_ROUNDF is floor(v + 0.5), so -12.5 rounds up to -12, not to -13.
        let pos = Vec3::new(-12.5, 0.0, -12.6);
        let reference = reference_position(pos);
        assert_eq!(reference, Vec3::new(-12.0, 0.0, -13.0));
    }

    #[test]
    fn a_non_vehicle_entity_projects_relative_to_the_observer() {
        let config = SyncConfig::new();
        let update = project_position(
            Vec3::new(12.0, 0.0, 13.0),
            reference_position(Vec3::new(10.0, 0.0, 10.0)),
            false,
            true,
            &config,
        );
        assert_eq!(update, PositionUpdate::PackedXZ { x: 2.0, z: 3.0 });
    }

    #[test]
    fn two_observers_receive_different_projections_of_the_same_entity() {
        let config = SyncConfig::new();
        let entity = Vec3::new(12.0, 0.0, 13.0);

        let near = project_position(
            entity,
            reference_position(Vec3::new(10.0, 0.0, 10.0)),
            false,
            true,
            &config,
        );
        let far = project_position(
            entity,
            reference_position(Vec3::new(100.0, 0.0, 100.0)),
            false,
            true,
            &config,
        );

        assert_eq!(near, PositionUpdate::PackedXZ { x: 2.0, z: 3.0 });
        assert_eq!(far, PositionUpdate::PackedXZ { x: -88.0, z: -87.0 });
    }

    #[test]
    fn the_self_observer_receives_its_own_fractional_offset() {
        // The watched entity is the observer itself: the reference is the
        // rounded own position, so the projection is only the fractional
        // offset from it.
        let config = SyncConfig::new();
        let self_pos = Vec3::new(12.5, 0.0, 13.5);
        let update = project_position(self_pos, reference_position(self_pos), false, true, &config);
        assert_eq!(update, PositionUpdate::PackedXZ { x: -0.5, z: -0.5 });
    }

    #[test]
    fn a_vehicle_entity_is_sent_absolute() {
        // entity.cpp:2754: isRelative is false for a vehicle entity, so the
        // projection ignores the observer's reference.
        let config = SyncConfig::new();
        let update = project_position(
            Vec3::new(12.0, 0.0, 13.0),
            reference_position(Vec3::new(10.0, 0.0, 10.0)),
            true,
            true,
            &config,
        );
        assert_eq!(update, PositionUpdate::PackedXZ { x: 12.0, z: 13.0 });
    }

    #[test]
    fn an_airborne_entity_packs_y_as_well() {
        let config = SyncConfig::new();
        let update = project_position(
            Vec3::new(12.0, 5.0, 13.0),
            reference_position(Vec3::new(10.0, 0.0, 10.0)),
            false,
            false,
            &config,
        );
        assert_eq!(
            update,
            PositionUpdate::PackedXYZ {
                x: 2.0,
                y: 5.0,
                z: 3.0
            }
        );
    }

    #[test]
    fn beyond_the_upper_limit_falls_back_to_a_detailed_update() {
        // entity.cpp:2763-2770: maxLimit is an exclusive upper bound, so a
        // relative position at or beyond it cannot be packed.
        let config = SyncConfig::new();
        let max_limit = config.max_limit();
        assert!((max_limit - 500.49).abs() < 0.01);

        let update = project_position(
            Vec3::new(max_limit + 1.0, 0.0, 0.0),
            Vec3::ZERO,
            false,
            true,
            &config,
        );
        assert_eq!(update, PositionUpdate::Detailed);
    }

    #[test]
    fn the_limit_edges_are_lower_inclusive_upper_exclusive() {
        // msgtypes.ipp:450: values must be in (minLimit, maxLimit); the check
        // in entity.cpp:2763 keeps minLimit itself packable and drops
        // maxLimit itself.
        let config = SyncConfig::new();
        let (min_limit, max_limit) = (config.min_limit(), config.max_limit());

        let at_min = project_position(
            Vec3::new(min_limit, 0.0, min_limit),
            Vec3::ZERO,
            false,
            true,
            &config,
        );
        assert_eq!(
            at_min,
            PositionUpdate::PackedXZ {
                x: min_limit,
                z: min_limit
            }
        );

        let at_max = project_position(
            Vec3::new(max_limit, 0.0, max_limit),
            Vec3::ZERO,
            false,
            true,
            &config,
        );
        assert_eq!(at_max, PositionUpdate::Detailed);
    }

    #[test]
    fn events_newer_than_the_observer_marker_are_selected_in_order() {
        // entity.cpp:2520-2586: events with number > lastEventNumber pass,
        // oldest first.
        let events = vec![
            event(10, EventLevel::Priority { priority: 0.0 }),
            event(11, EventLevel::Priority { priority: 0.0 }),
            event(12, EventLevel::Priority { priority: 0.0 }),
        ];

        let selected = select_events(&events, 11, f64::NEG_INFINITY, i32::MAX);
        assert_eq!(
            selected.iter().map(|e| e.number).collect::<Vec<_>>(),
            vec![12]
        );

        let all = select_events(&events, 9, f64::NEG_INFINITY, i32::MAX);
        assert_eq!(
            all.iter().map(|e| e.number).collect::<Vec<_>>(),
            vec![10, 11, 12]
        );
    }

    #[test]
    fn detail_properties_send_to_observers_detailed_enough() {
        // history_event.hpp:47: detailLevel <= detail. A low detail level is
        // the most detailed, so it receives every property whose level is at
        // or above its own.
        let events = vec![
            event(1, EventLevel::Detail { level: 0 }),
            event(2, EventLevel::Detail { level: 2 }),
        ];

        let selected = select_events(&events, 0, f64::NEG_INFINITY, 0);
        assert_eq!(
            selected.iter().map(|e| e.number).collect::<Vec<_>>(),
            vec![1, 2]
        );

        let selected = select_events(&events, 0, f64::NEG_INFINITY, 1);
        assert_eq!(
            selected.iter().map(|e| e.number).collect::<Vec<_>>(),
            vec![2]
        );
    }

    #[test]
    fn priority_messages_send_only_above_the_threshold() {
        // history_event.hpp:48: threshold < priority.
        let events = vec![
            event(1, EventLevel::Priority { priority: 1.0 }),
            event(2, EventLevel::Priority { priority: 100.0 }),
        ];

        let selected = select_events(&events, 0, 1.0, i32::MAX);
        assert_eq!(
            selected.iter().map(|e| e.number).collect::<Vec<_>>(),
            vec![2]
        );
    }

    #[test]
    fn a_reliable_position_rides_with_the_property_events() {
        // entity.cpp:2538: isReliable = hasEventsToSend.
        let entity_events = vec![event(5, EventLevel::Priority { priority: 0.0 })];
        let config = SyncConfig::new();
        let entity_pos = Vec3::new(12.0, 0.0, 13.0);

        let mut view = ObserverView::new(Vec3::new(10.0, 0.0, 10.0));
        view.last_event_number = 4;
        let with_events = ClientUpdate::build(entity_pos, &entity_events, view, &config);
        assert!(with_events.is_reliable);
        assert_eq!(with_events.events.len(), 1);

        let mut view = ObserverView::new(Vec3::new(10.0, 0.0, 10.0));
        view.last_event_number = 5;
        let without_events = ClientUpdate::build(entity_pos, &entity_events, view, &config);
        assert!(!without_events.is_reliable);
        assert!(without_events.events.is_empty());
    }

    #[test]
    fn self_and_another_observer_each_receive_their_own_update() {
        // BW-08-03-04 acceptance: the watched entity and a second observer
        // both receive the same properties, but each measures the position
        // from its own reference.
        let config = SyncConfig::new();
        let entity_pos = Vec3::new(12.0, 0.0, 13.0);
        let entity_events = vec![
            event(7, EventLevel::Detail { level: 0 }),
            event(8, EventLevel::Detail { level: 3 }),
        ];

        // The self observer: its own update, own reference (12, 0, 13), so
        // its projection is the zero offset.
        let mut self_view = ObserverView::new(entity_pos);
        self_view.last_event_number = 6;
        let self_update = ClientUpdate::build(entity_pos, &entity_events, self_view, &config);
        assert_eq!(
            self_update.position,
            PositionUpdate::PackedXZ { x: 0.0, z: 0.0 }
        );

        // A second observer 100 metres away: same properties, its own
        // projection (reference (112, 0, 113)).
        let mut other_view = ObserverView::new(Vec3::new(112.0, 0.0, 113.0));
        other_view.last_event_number = 6;
        let other_update = ClientUpdate::build(entity_pos, &entity_events, other_view, &config);
        assert_eq!(
            other_update.position,
            PositionUpdate::PackedXZ {
                x: -100.0,
                z: -100.0
            }
        );
        assert_eq!(self_update.events, other_update.events);
        assert_ne!(self_update.position, other_update.position);
    }

    #[test]
    fn a_less_detailed_observer_receives_fewer_properties() {
        // The projections are per-observer, and so is the property selection:
        // the closest observer (detail level 0) receives every property, while
        // a less detailed observer misses the level-0 property.
        let config = SyncConfig::new();
        let entity_pos = Vec3::new(12.0, 0.0, 13.0);
        let entity_events = vec![
            event(7, EventLevel::Detail { level: 0 }),
            event(8, EventLevel::Detail { level: 3 }),
        ];

        let mut self_view = ObserverView::new(entity_pos);
        self_view.last_event_number = 6;
        self_view.detail_level = 0;
        let self_update = ClientUpdate::build(entity_pos, &entity_events, self_view, &config);

        let mut far_view = ObserverView::new(Vec3::new(112.0, 0.0, 113.0));
        far_view.last_event_number = 6;
        far_view.detail_level = 2;
        let far_update = ClientUpdate::build(entity_pos, &entity_events, far_view, &config);

        assert_eq!(self_update.events.len(), 2);
        assert_eq!(far_update.events.len(), 1);
        assert_eq!(far_update.events[0].number, 8);
    }
}
