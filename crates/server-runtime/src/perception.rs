//! BW-08-06-01: proximity traps and vision.
//!
//! A `ProximityController` (`proximity_controller.hpp:29`) is a controller that
//! notifies its entity whenever another entity crosses the boundary of a region
//! around it. The region is a *square*, not a circle — an entity is within it
//! when its position is within `range` on both the x and the z axis
//! (`proximity_controller.cpp:38-39`). Crossing the boundary fires
//! `onEnterTrap`; leaving fires `onLeaveTrap`; and on an onload where a trapped
//! entity is gone from the cell only its id is known, so `onLeaveTrapID` fires
//! (`proximity_controller.cpp:363-386`). The membership is the half-open
//! interval `lower < p <= upper` on each axis (`range_trigger.hpp:110`, `:138`)
//! — the same boundary already modelled by `range_list::RangeTrigger`.
//!
//! A `VisionController` (`vision_controller.hpp:23`) is a periodic updatable:
//! every `updatePeriod` ticks (10 by default) `updateVisibleEntities`
//! (`entity_vision.cpp:258`) recomputes which entities are seen. A candidate is
//! seen when it is inside the vision range square (`range_trigger.hpp:110`,
//! `:138`), `canBeSeen` is on, the two-point line-of-sight `collide` finds no
//! obstacle (`entity_vision.cpp:303-311`), and the angle to the entity's head is
//! strictly within the field of view (`:316-323`). The set difference against
//! the previous visible set fires `onStartSeeing` and `onStopSeeing`
//! (`entity_vision.cpp:346-377`).
//!
//! The acceptance, "进出区域和遮挡回调吻合", is that the region enter/leave and
//! the occlusion callbacks fire exactly when the boundary, the sight line, or
//! the field of view says so.
//!
//! Reference:
//! - `proximity_controller.cpp:27-67` — `ProximityRangeTrigger` and the
//!   `FLAG_ENTITY_TRIGGER` box.
//! - `proximity_controller.cpp:284-306` — `setRange`: at range 0 the trigger is
//!   deleted (which drops the members through `onLeaveTrap`).
//! - `proximity_controller.cpp:310-360` — `triggerEnter`/`triggerLeave` call
//!   `onEnterTrap`/`onLeaveTrap`.
//! - `proximity_controller.cpp:363-386` — `onLeaveTrapID` for an entity that
//!   cannot be resolved on an onload.
//! - `proximity_controller.cpp:149-213` — `OnloadHandler`: matched ids give no
//!   callback, unmatched ones leave, `nonstandardLeave` when the entity is not
//!   on the cell at all.
//! - `entity_vision.cpp:258-401` — `updateVisibleEntities`: the range gate,
//!   `canBeSeen`, the two-point collide, the field-of-view angle, and the
//!   start/stop diff.
//! - `entity_vision.cpp:419-448` — `setVisibleEntities`: an id the cell cannot
//!   find fires `onStopSeeingID`.
//! - `vision_controller.cpp:158-177` — `update` only passes every `updatePeriod`
//!   ticks.

use std::collections::BTreeSet;

use crate::movement::Vec3;
use crate::range_list::{EntityId, RangeList, TriggerEvent};

/// A segment of the chunk space's geometry, used for line-of-sight tests
/// (`ChunkSpace::collide`, `entity_vision.cpp:303`). A wall is a 2D segment in
/// the ground plane, optionally spanning only a band of heights.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Wall {
    /// One endpoint.
    pub x1: f64,
    /// One endpoint.
    pub z1: f64,
    /// The other endpoint.
    pub x2: f64,
    /// The other endpoint.
    pub z2: f64,
    /// The lowest height the wall blocks; `-inf` for a ground wall.
    pub y_min: f64,
    /// The highest height the wall blocks; `inf` for a ground wall.
    pub y_max: f64,
}

impl Wall {
    /// A ground wall, blocking every height (`Wall::new`).
    pub fn new(x1: f64, z1: f64, x2: f64, z2: f64) -> Self {
        Wall {
            x1,
            z1,
            x2,
            z2,
            y_min: f64::NEG_INFINITY,
            y_max: f64::INFINITY,
        }
    }

    /// A wall blocking only the height band `y_min..y_max` — an overhang
    /// (`Wall::new_between`).
    pub fn band(x1: f64, z1: f64, x2: f64, z2: f64, y_min: f64, y_max: f64) -> Self {
        Wall {
            x1,
            z1,
            x2,
            z2,
            y_min,
            y_max,
        }
    }

    /// `ChunkSpace::collide` (`entity_vision.cpp:303`): the distance along the
    /// `from`->`to` ray to the first wall it crosses, or 0.0 when the sight line
    /// is clear.
    pub fn collide(&self, from: Vec3, to: Vec3) -> f64 {
        let dx = to.x - from.x;
        let dz = to.z - from.z;
        let len = dx * dx + dz * dz;
        if len == 0.0 {
            return 0.0;
        }
        let len = len.sqrt();
        // Parametric ray `a + t*d` and wall `c + s*e`.
        let ex = self.x2 - self.x1;
        let ez = self.z2 - self.z1;
        let denom = ex * dz - dx * ez;
        if denom.abs() < f64::EPSILON {
            return 0.0; // parallel
        }
        let t = (ex * (self.z1 - from.z) - ez * (self.x1 - from.x)) / denom;
        let s = (dx * (self.z1 - from.z) - dz * (self.x1 - from.x)) / denom;
        // The wall is crossed from inside the ray outward.
        if !(t > 0.0 && t <= 1.0) || !(0.0..=1.0).contains(&s) {
            return 0.0;
        }
        let y = from.y + t * (to.y - from.y);
        if !(self.y_min..=self.y_max).contains(&y) {
            return 0.0; // the wall is above or below the ray
        }
        t * len
    }
}

/// The chunk space's geometry: a set of walls to test sight lines against
/// (`ChunkSpace::collide`, `entity_vision.cpp:303`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Occluder {
    /// The walls.
    pub walls: Vec<Wall>,
}

impl Occluder {
    /// An empty occluder.
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a wall.
    pub fn with_wall(mut self, wall: Wall) -> Self {
        self.walls.push(wall);
        self
    }

    /// `ChunkSpace::collide` (`entity_vision.cpp:303`): the distance along the
    /// sight line to the first wall, or 0.0 when nothing blocks it. `dist > 0`
    /// means the sight is blocked.
    pub fn collide(&self, from: Vec3, to: Vec3) -> f64 {
        let mut best = 0.0;
        for wall in &self.walls {
            let dist = wall.collide(from, to);
            if dist > 0.0 && (best == 0.0 || dist < best) {
                best = dist;
            }
        }
        best
    }
}

/// One candidate entity for a vision pass, at its dropped position.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SeenEntity {
    /// The entity id.
    pub id: EntityId,
    /// The dropped position.
    pub position: Vec3,
    /// The height others see up to (`visibleHeight`).
    pub visible_height: f64,
    /// `EntityVision::canBeSeen` (`entity_vision.cpp:294`).
    pub can_be_seen: bool,
}

/// A vision callback, in the order it fired
/// (`onStartSeeing`/`onStopSeeing`, `entity_vision.cpp:350-377`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VisionEvent {
    /// The entity became visible this pass (`onStartSeeing`).
    StartSeeing(EntityId),
    /// The entity stopped being visible this pass (`onStopSeeing`).
    StopSeeing(EntityId),
    /// A visible entity the cell cannot resolve on an onload
    /// (`onStopSeeingID`, `entity_vision.cpp:442`).
    StopSeeingID(EntityId),
}

/// The field of view, vision range and period of one entity's vision
/// (`EntityVision` + `VisionController`).
#[derive(Debug, Clone, PartialEq)]
pub struct Vision {
    /// The half-angle of the field of view (`visionAngle_`, `vision_controller.hpp:64`).
    vision_angle: f64,
    /// The vision range (`visionRange_`, `vision_controller.hpp:65`); the range
    /// square (`range_trigger.hpp:110`, `:138`).
    vision_range: f64,
    /// The height the eye looks from (`seeingHeight_`, `vision_controller.hpp:66`).
    seeing_height: f64,
    /// The period in ticks (`updatePeriod_`, `vision_controller.hpp:67`, 10 by
    /// default).
    update_period: u32,
    /// Ticks since the last pass (`tickSinceLast_`, `vision_controller.cpp:84`).
    tick_since_last: u32,
    /// The visible entities (`visibleEntities_`, `entity_vision.hpp:83`).
    visible: BTreeSet<EntityId>,
    /// The events queued this call (`onStartSeeing`/`onStopSeeing`).
    events: Vec<VisionEvent>,
}

impl Default for Vision {
    /// The defaults of `VisionController` (`vision_controller.hpp:53`).
    fn default() -> Self {
        Self::new()
    }
}

impl Vision {
    /// A `VisionController` (`vision_controller.hpp:53`): `visionAngle = 1`,
    /// `visionRange = 20`, `seeingHeight = 2`, `updatePeriod = 10`.
    pub fn new() -> Self {
        Vision {
            vision_angle: 1.0,
            vision_range: 20.0,
            seeing_height: 2.0,
            update_period: 10,
            tick_since_last: 0,
            visible: BTreeSet::new(),
            events: Vec::new(),
        }
    }

    /// The field of view (`visionAngle()`, `vision_controller.hpp:59`).
    pub fn vision_angle(&self) -> f64 {
        self.vision_angle
    }

    /// Set the field of view and range (`setVisionRange`,
    /// `vision_controller.hpp:69`).
    pub fn set_vision_range(&mut self, vision_angle: f64, vision_range: f64) {
        self.vision_angle = vision_angle;
        self.vision_range = vision_range;
    }

    /// The period (`updatePeriod_`, `vision_controller.cpp:162`).
    pub fn update_period(&self) -> u32 {
        self.update_period
    }

    /// The visible entities (`visibleEntities_`, `entity_vision.hpp:84`).
    pub fn visible(&self) -> &BTreeSet<EntityId> {
        &self.visible
    }

    /// `VisionController::update` (`vision_controller.cpp:158`): the pass runs
    /// only every `updatePeriod` ticks; the events queued by this call are
    /// returned.
    pub fn update(
        &mut self,
        eye: Vec3,
        yaw: f64,
        candidates: &[SeenEntity],
        occluder: &Occluder,
    ) -> &[VisionEvent] {
        let start = self.events.len();
        if self.tick_since_last >= self.update_period {
            self.tick_since_last = 0;
            self.update_visible(eye, yaw, candidates, occluder);
        } else {
            self.tick_since_last += 1;
        }
        &self.events[start..]
    }

    /// `EntityVision::setVisibleEntities` (`entity_vision.cpp:419`): the
    /// streamed visible set on an onload; an id the cell cannot resolve fires
    /// `onStopSeeingID`.
    pub fn set_visible_entities(
        &mut self,
        ids: &[EntityId],
        known: &BTreeSet<EntityId>,
    ) -> Vec<VisionEvent> {
        let mut events = Vec::new();
        for &id in ids {
            if known.contains(&id) {
                self.visible.insert(id);
            } else {
                self.events.push(VisionEvent::StopSeeingID(id));
                events.push(VisionEvent::StopSeeingID(id));
            }
        }
        events
    }

    /// `EntityVision::updateVisibleEntities` (`entity_vision.cpp:258`).
    fn update_visible(
        &mut self,
        eye: Vec3,
        yaw: f64,
        candidates: &[SeenEntity],
        occluder: &Occluder,
    ) {
        let mut head = eye;
        head.y += self.seeing_height;
        // `setPitchYaw(0, yaw)` (`entity_vision.cpp:283`).
        let direction = Vec3::new(yaw.sin(), 0.0, yaw.cos());

        let mut sorted: Vec<&SeenEntity> = candidates.iter().collect();
        sorted.sort_by_key(|c| c.id);
        let mut new_visible = BTreeSet::new();
        for candidate in sorted {
            if !candidate.can_be_seen {
                continue;
            }
            // The vision range square (`range_trigger.hpp:110`, `:138`) on the
            // ground axes.
            let ex = eye.x - self.vision_range < candidate.position.x
                && candidate.position.x <= eye.x + self.vision_range;
            let ez = eye.z - self.vision_range < candidate.position.z
                && candidate.position.z <= eye.z + self.vision_range;
            if !ex || !ez {
                continue;
            }
            let mut can_see = false;
            for attempt in 0..2 {
                let height = if attempt == 0 {
                    candidate.visible_height
                } else {
                    0.75 * candidate.visible_height
                };
                let target = Vec3::new(
                    candidate.position.x,
                    candidate.position.y + height,
                    candidate.position.z,
                );
                // `dist > 0` means the sight line is blocked
                // (`entity_vision.cpp:308`).
                if occluder.collide(head, target) > 0.0 {
                    continue;
                }
                let to = target.minus(head).unit();
                let angle = direction.dot(to).clamp(-1.0, 1.0).acos();
                if angle < self.vision_angle {
                    can_see = true;
                    break;
                }
            }
            if can_see {
                new_visible.insert(candidate.id);
            }
        }

        // The start/stop diff over the sorted old and new sets
        // (`entity_vision.cpp:346-377`).
        let old = std::mem::take(&mut self.visible);
        let mut events = Vec::new();
        let mut oi = old.iter().copied().peekable();
        let mut ni = new_visible.iter().copied().peekable();
        loop {
            match (oi.peek(), ni.peek()) {
                (Some(&o), Some(&n)) => {
                    if o == n {
                        oi.next();
                        ni.next();
                    } else if o < n {
                        events.push(VisionEvent::StopSeeing(o));
                        oi.next();
                    } else {
                        events.push(VisionEvent::StartSeeing(n));
                        ni.next();
                    }
                }
                (Some(&o), None) => {
                    events.push(VisionEvent::StopSeeing(o));
                    oi.next();
                }
                (None, Some(&n)) => {
                    events.push(VisionEvent::StartSeeing(n));
                    ni.next();
                }
                (None, None) => break,
            }
        }
        self.visible = new_visible;
        self.events.extend(events);
    }
}

/// A trap callback, in the order it fired
/// (`onEnterTrap`/`onLeaveTrap`/`onLeaveTrapID`, `proximity_controller.cpp:313`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrapEvent {
    /// An entity entered the region (`onEnterTrap`,
    /// `proximity_controller.cpp:340`).
    EnterTrap(EntityId),
    /// An entity left the region and was found on the cell (`onLeaveTrap`,
    /// `proximity_controller.cpp:352`).
    LeaveTrap(EntityId),
    /// An entity left the region and was not on the cell (`onLeaveTrapID`,
    /// `proximity_controller.cpp:371`).
    LeaveTrapID(EntityId),
}

/// A proximity trap on one entity (`ProximityController` +
/// `ProximityRangeTrigger`).
#[derive(Debug)]
pub struct ProximityTrap {
    /// The controller id (`id_`, `proximity_controller.hpp:75`).
    pub controller_id: i32,
    /// The optional user arg (`userArg()`, `proximity_controller.cpp:340`).
    pub user_arg: i32,
    /// The region's centre (`pRangeListNode()`, `proximity_controller.cpp:35`).
    subject: EntityId,
    /// The range (`range_`, `proximity_controller.cpp:76`).
    range: f64,
    /// The cell's range list, which owns the membership and the boundary
    /// transitions (`RangeList`).
    list: RangeList,
    /// The events from the range list already translated into trap callbacks.
    seen: usize,
}

impl ProximityTrap {
    /// `ProximityController::New` (`proximity_controller.cpp:113`): the factory
    /// refuses a range of 0 or less. The trigger itself is installed by
    /// [`start`](Self::start) or [`start_onloaded`](Self::start_onloaded).
    pub fn new(controller_id: i32, user_arg: i32, subject: EntityId, range: f64) -> Option<Self> {
        if range <= 0.0 {
            return None;
        }
        Some(ProximityTrap {
            controller_id,
            user_arg,
            subject,
            range,
            list: RangeList::new(),
            seen: 0,
        })
    }

    /// The region's centre.
    pub fn subject(&self) -> EntityId {
        self.subject
    }

    /// The range.
    pub fn range(&self) -> f64 {
        self.range
    }

    /// The cell's range list — an entity is brought in here and moved through
    /// it.
    pub fn list_mut(&mut self) -> &mut RangeList {
        &mut self.list
    }

    /// `ProximityController::startReal` on an initial start
    /// (`proximity_controller.cpp:131-137`): `setRange(range_)` installs the
    /// trigger, whose insert sweep fires `onEnterTrap` for the entities already
    /// inside — a trap can fire before `addProximity` even returns
    /// (`proximity_controller.cpp:38-39`).
    pub fn start(&mut self) {
        self.list.add_trigger(self.subject, self.range);
    }

    /// Re-issue the trap callbacks for the range list events since the last
    /// call (`triggerEnter`/`triggerLeave`, `proximity_controller.cpp:313`).
    pub fn poll(&mut self) -> Vec<TrapEvent> {
        let mut events = Vec::new();
        for event in &self.list.events()[self.seen..] {
            match *event {
                TriggerEvent::Enter(id) => events.push(TrapEvent::EnterTrap(id)),
                TriggerEvent::Leave(id) => events.push(TrapEvent::LeaveTrap(id)),
            }
        }
        self.seen = self.list.events().len();
        events
    }

    /// `ProximityController::setRange` (`proximity_controller.cpp:284`): at
    /// range 0 the trigger is deleted, dropping the members through
    /// `onLeaveTrap`; otherwise the box grows or shrinks.
    pub fn set_range(&mut self, range: f64) {
        if range <= 0.0 {
            self.list.remove_trigger(self.subject);
            self.range = 0.0;
        } else {
            self.range = range;
            self.list.set_trigger_range(self.subject, range);
        }
    }

    /// `ProximityController::stopReal` (`proximity_controller.cpp:218`): the
    /// trigger is deleted with the leave callbacks swallowed by the no-op
    /// handler.
    pub fn stop(&mut self) {
        self.list.remove_trigger(self.subject);
        self.seen = self.list.events().len();
    }

    /// `ProximityController::startReal` on an onload
    /// (`proximity_controller.cpp:96-135`): the trigger is installed fresh and
    /// the streamed set is reconciled against the entities now on the cell. An
    /// id that re-entered gives no callback; an id that did not leaves, with
    /// `onLeaveTrapID` when it is not on the cell at all.
    pub fn start_onloaded(&mut self, ids: &[EntityId]) -> Vec<TrapEvent> {
        // `pProximityTrigger_->init()` (`proximity_controller.cpp:122`): the
        // insert sweep fires an enter for every entity now inside.
        self.list.add_trigger(self.subject, self.range);

        let mut matched = vec![false; ids.len()];
        let mut events = Vec::new();
        for event in &self.list.events()[self.seen..] {
            if let TriggerEvent::Enter(id) = *event {
                if let Some(index) = ids.iter().position(|&x| x == id) {
                    matched[index] = true;
                } else {
                    events.push(TrapEvent::EnterTrap(id));
                }
            }
        }
        self.seen = self.list.events().len();
        for (index, &id) in ids.iter().enumerate() {
            if matched[index] {
                continue;
            }
            if self.list.entities().contains_key(&id) {
                events.push(TrapEvent::LeaveTrap(id));
            } else {
                events.push(TrapEvent::LeaveTrapID(id));
            }
        }
        events
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::range_list::Position;

    fn e(id: u32) -> EntityId {
        id
    }

    fn at(x: f64, z: f64) -> SeenEntity {
        SeenEntity {
            id: e(2),
            position: Vec3::new(x, 0.0, z),
            visible_height: 2.0,
            can_be_seen: true,
        }
    }

    fn ids(events: &[TrapEvent]) -> Vec<EntityId> {
        events
            .iter()
            .map(|event| match *event {
                TrapEvent::EnterTrap(id)
                | TrapEvent::LeaveTrap(id)
                | TrapEvent::LeaveTrapID(id) => id,
            })
            .collect()
    }

    /// A vision whose pass runs on every call, for the tests that are not
    /// about the period.
    fn vision_now() -> Vision {
        let mut vision = Vision::new();
        vision.update_period = 0;
        vision
    }

    /// The acceptance, part one: the region enter and leave callbacks fire at
    /// the square boundary.
    #[test]
    fn the_trap_fires_enter_and_leave_at_the_square_boundary() {
        let mut trap = ProximityTrap::new(1, 0, e(1), 10.0).expect("a positive range");
        trap.list_mut().add_entity(e(2), Position::new(10.0, 0.0));
        trap.start();

        // The upper edge (p == upper) is inside.
        assert_eq!(trap.poll(), vec![TrapEvent::EnterTrap(e(2))]);

        // Crossing out through the lower edge (p == lower is outside).
        trap.list_mut().move_entity(e(2), Position::new(-10.0, 0.0));
        assert_eq!(trap.poll(), vec![TrapEvent::LeaveTrap(e(2))]);
    }

    /// A trap fires immediately for the entities already inside
    /// (`proximity_controller.cpp:38-39`).
    #[test]
    fn a_trap_fires_immediately_for_the_entities_already_inside() {
        let mut trap = ProximityTrap::new(1, 0, e(1), 10.0).expect("a positive range");
        trap.list_mut().add_entity(e(2), Position::new(1.0, 0.0));
        trap.list_mut().add_entity(e(3), Position::new(50.0, 0.0));
        trap.start();

        // Only e(2) is inside the box at the insert sweep.
        assert_eq!(trap.poll(), vec![TrapEvent::EnterTrap(e(2))]);
    }

    /// Growing the range fires enters for the entities it grows over; shrinking
    /// fires leaves for the ones it drops.
    #[test]
    fn growing_and_shrinking_the_range_fire_symmetrically() {
        let mut trap = ProximityTrap::new(1, 0, e(1), 10.0).expect("a positive range");
        trap.list_mut().add_entity(e(2), Position::new(15.0, 0.0));
        trap.start();
        assert!(trap.poll().is_empty(), "outside the initial range");

        trap.set_range(20.0);
        assert_eq!(trap.poll(), vec![TrapEvent::EnterTrap(e(2))]);

        trap.set_range(10.0);
        assert_eq!(trap.poll(), vec![TrapEvent::LeaveTrap(e(2))]);
    }

    /// A range of 0 deletes the trigger and reports the members as leaving.
    #[test]
    fn a_zero_range_deletes_the_trigger_and_reports_the_members() {
        let mut trap = ProximityTrap::new(1, 0, e(1), 10.0).expect("a positive range");
        trap.list_mut().add_entity(e(2), Position::new(1.0, 0.0));
        trap.start();
        assert_eq!(trap.poll(), vec![TrapEvent::EnterTrap(e(2))]);

        trap.set_range(0.0);
        assert_eq!(trap.poll(), vec![TrapEvent::LeaveTrap(e(2))]);
    }

    /// `stopReal` deletes the trigger with the leave callbacks swallowed by the
    /// no-op handler (`proximity_controller.cpp:218`).
    #[test]
    fn stopping_swallows_the_leave_callbacks() {
        let mut trap = ProximityTrap::new(1, 0, e(1), 10.0).expect("a positive range");
        trap.list_mut().add_entity(e(2), Position::new(1.0, 0.0));
        trap.start();
        assert_eq!(trap.poll(), vec![TrapEvent::EnterTrap(e(2))]);

        trap.stop();
        assert!(trap.poll().is_empty());
    }

    /// The factory refuses a range of 0 or less
    /// (`proximity_controller.cpp:115`).
    #[test]
    fn a_non_positive_range_is_refused() {
        assert!(ProximityTrap::new(1, 0, e(1), 0.0).is_none());
        assert!(ProximityTrap::new(1, 0, e(1), -5.0).is_none());
    }

    /// The onload reconciliation: a matched id gives no callback, an id that
    /// did not re-enter leaves by id when the cell cannot resolve it and by
    /// entity when it can (`proximity_controller.cpp:149-213`).
    #[test]
    fn the_onload_set_is_reconciled_against_the_cell() {
        let mut trap = ProximityTrap::new(1, 0, e(1), 10.0).expect("a positive range");
        trap.list_mut().add_entity(e(2), Position::new(1.0, 0.0));
        trap.list_mut().add_entity(e(4), Position::new(3.0, 0.0));
        trap.list_mut().add_entity(e(6), Position::new(50.0, 0.0));

        // e(2) re-entered, e(3) and e(5) are gone from the cell, e(4) is inside
        // but was not in the set, e(6) is on the cell but outside the box.
        let events = trap.start_onloaded(&[e(2), e(3), e(5), e(6)]);
        assert_eq!(
            events,
            vec![
                TrapEvent::EnterTrap(e(4)),
                TrapEvent::LeaveTrapID(e(3)),
                TrapEvent::LeaveTrapID(e(5)),
                TrapEvent::LeaveTrap(e(6)),
            ]
        );
        assert_eq!(ids(&events), vec![e(4), e(3), e(5), e(6)]);
    }

    /// The acceptance, part two: the occlusion callbacks fire when the sight
    /// line or the field of view says so.
    #[test]
    fn vision_fires_start_and_stop_with_occlusion_and_the_fov() {
        let mut vision = vision_now();
        let occluder = Occluder::new();

        // Straight ahead along +z (yaw 0): seen.
        let events = vision.update(Vec3::ZERO, 0.0, &[at(0.0, 10.0)], &occluder);
        assert_eq!(events, vec![VisionEvent::StartSeeing(e(2))]);
        assert_eq!(
            vision.visible().iter().copied().collect::<Vec<_>>(),
            vec![e(2)]
        );

        // Behind: outside the field of view, stopped.
        let events = vision.update(
            Vec3::ZERO,
            std::f64::consts::PI,
            &[at(0.0, 10.0)],
            &occluder,
        );
        assert_eq!(events, vec![VisionEvent::StopSeeing(e(2))]);

        // A wall between: blocked. It was already stopped, so no new callback.
        let wall = Occluder::new().with_wall(Wall::new(-10.0, 5.0, 10.0, 5.0));
        let events = vision.update(Vec3::ZERO, 0.0, &[at(0.0, 10.0)], &wall);
        assert_eq!(events, &[]);

        // The wall gone: seen again.
        let events = vision.update(Vec3::ZERO, 0.0, &[at(0.0, 10.0)], &occluder);
        assert_eq!(events, vec![VisionEvent::StartSeeing(e(2))]);
    }

    /// The field of view is a half-angle around the facing: inside it is
    /// seen, outside it is not.
    #[test]
    fn the_fov_bounds_what_is_seen() {
        let occluder = Occluder::new();

        // At 0.4 radians off the facing: inside the 0.5 half-angle.
        let mut vision = vision_now();
        vision.set_vision_range(0.5, 20.0);
        let angle: f64 = 0.4;
        let target = Vec3::new(10.0 * angle.sin(), 0.0, 10.0 * angle.cos());
        assert_eq!(
            vision.update(Vec3::ZERO, 0.0, &[at(target.x, target.z)], &occluder),
            vec![VisionEvent::StartSeeing(e(2))]
        );

        // At 0.6 radians off the facing: outside the half-angle.
        let mut vision = vision_now();
        vision.set_vision_range(0.5, 20.0);
        let angle: f64 = 0.6;
        let target = Vec3::new(10.0 * angle.sin(), 0.0, 10.0 * angle.cos());
        assert_eq!(
            vision.update(Vec3::ZERO, 0.0, &[at(target.x, target.z)], &occluder),
            &[]
        );
        assert!(vision.visible().is_empty());
    }

    /// The range square bounds the candidates: an entity beyond it is not seen.
    #[test]
    fn the_vision_range_square_bounds_the_candidates() {
        let mut vision = vision_now();
        vision.set_vision_range(5.0, 5.0);
        let occluder = Occluder::new();

        assert_eq!(
            vision.update(Vec3::ZERO, 0.0, &[at(0.0, 6.0)], &occluder),
            &[]
        );
        assert_eq!(
            vision.update(Vec3::ZERO, 0.0, &[at(4.0, 4.0)], &occluder),
            vec![VisionEvent::StartSeeing(e(2))]
        );
    }

    /// `canBeSeen` off means the entity is never seen
    /// (`entity_vision.cpp:294`).
    #[test]
    fn an_unseen_entity_is_never_visible() {
        let mut vision = vision_now();
        let occluder = Occluder::new();

        let mut candidate = at(10.0, 0.0);
        candidate.can_be_seen = false;
        assert_eq!(vision.update(Vec3::ZERO, 0.0, &[candidate], &occluder), &[]);
        assert!(vision.visible().is_empty());
    }

    /// The two-point line of sight: a wall band that blocks the ray to the
    /// head but not the ray to the chest at three quarters height leaves the
    /// entity seen (`entity_vision.cpp:325-327`).
    #[test]
    fn the_second_sight_point_sees_under_an_overhang() {
        let mut vision = vision_now();
        // A band at heights 1.8..2.0 crossing the +z ray at z = 5.
        let occluder = Occluder::new().with_wall(Wall::band(-10.0, 5.0, 10.0, 5.0, 1.8, 2.0));

        assert_eq!(
            vision.update(Vec3::ZERO, 0.0, &[at(0.0, 10.0)], &occluder),
            vec![VisionEvent::StartSeeing(e(2))]
        );
    }

    /// The pass runs only every `updatePeriod` ticks
    /// (`vision_controller.cpp:162`).
    #[test]
    fn the_pass_runs_only_every_period_ticks() {
        let mut vision = Vision::new();
        let occluder = Occluder::new();

        // The first ten calls only count ticks; the eleventh runs the pass.
        for _ in 0..10 {
            assert_eq!(
                vision.update(Vec3::ZERO, 0.0, &[at(0.0, 10.0)], &occluder),
                &[]
            );
        }
        assert_eq!(
            vision.update(Vec3::ZERO, 0.0, &[at(0.0, 10.0)], &occluder),
            vec![VisionEvent::StartSeeing(e(2))]
        );
    }

    /// The onload visible set: an id the cell cannot resolve stops by id
    /// (`entity_vision.cpp:419-448`).
    #[test]
    fn the_onload_visible_set_resolves_ids_against_the_cell() {
        let mut vision = Vision::new();
        let mut known = BTreeSet::new();
        known.insert(e(2));

        let events = vision.set_visible_entities(&[e(2), e(3)], &known);
        assert_eq!(events, vec![VisionEvent::StopSeeingID(e(3))]);
        assert_eq!(
            vision.visible().iter().copied().collect::<Vec<_>>(),
            vec![e(2)]
        );
    }

    /// A degenerate ray never blocks: zero length, a wall parallel to the
    /// ray, and a wall the ray never reaches or misses sideways all read
    /// clear.
    #[test]
    fn degenerate_rays_read_clear() {
        let wall = Wall::new(-10.0, 5.0, 10.0, 5.0);

        // Zero-length ray.
        assert_eq!(wall.collide(Vec3::ZERO, Vec3::ZERO), 0.0, "no ray, no hit");

        // A wall parallel to the +z ray.
        let parallel = Wall::new(5.0, -10.0, 5.0, 10.0);
        assert_eq!(
            parallel.collide(Vec3::ZERO, Vec3::new(0.0, 0.0, 10.0)),
            0.0,
            "parallel lines never meet"
        );

        // The wall lies behind the ray, and then beside it.
        let behind = Wall::new(-10.0, -5.0, 10.0, -5.0);
        assert_eq!(
            behind.collide(Vec3::ZERO, Vec3::new(0.0, 0.0, 10.0)),
            0.0,
            "the crossing is outside the ray"
        );
        let beside = Wall::new(5.0, 5.0, 15.0, 5.0);
        assert_eq!(
            beside.collide(Vec3::ZERO, Vec3::new(0.0, 0.0, 10.0)),
            0.0,
            "the crossing is outside the wall segment"
        );
    }

    /// The defaults are the `VisionController` defaults
    /// (`vision_controller.hpp:53`).
    #[test]
    fn the_defaults_match_the_vision_controller() {
        let vision = Vision::default();
        assert_eq!(vision.vision_angle(), 1.0);
        assert_eq!(vision.update_period(), 10);
        assert_eq!(vision.vision_range, 20.0);
        assert_eq!(vision.seeing_height, 2.0);
        assert!(vision.visible().is_empty());
    }

    /// The trap reports its subject and range.
    #[test]
    fn the_trap_reports_its_subject_and_range() {
        let trap = ProximityTrap::new(7, 3, e(1), 10.0).expect("a positive range");
        assert_eq!(trap.subject(), e(1));
        assert_eq!(trap.range(), 10.0);
    }

    /// The start/stop diff reports only the change between the two sorted
    /// visible sets (`entity_vision.cpp:346-377`).
    #[test]
    fn an_update_reports_only_the_change_in_the_visible_set() {
        fn seen(id: u32, z: f64) -> SeenEntity {
            SeenEntity {
                id: e(id),
                position: Vec3::new(0.0, 0.0, z),
                visible_height: 2.0,
                can_be_seen: true,
            }
        }

        let mut vision = vision_now();
        let occluder = Occluder::new();

        // Entity 5 ahead: seen.
        assert_eq!(
            vision.update(Vec3::ZERO, 0.0, &[seen(5, 10.0)], &occluder),
            vec![VisionEvent::StartSeeing(e(5))]
        );

        // Entity 1 joins it: only the join is reported, entity 5 stays seen.
        assert_eq!(
            vision.update(Vec3::ZERO, 0.0, &[seen(1, 15.0), seen(5, 10.0)], &occluder),
            vec![VisionEvent::StartSeeing(e(1))]
        );
        assert_eq!(
            vision.visible().iter().copied().collect::<Vec<_>>(),
            vec![e(1), e(5)]
        );

        // Entity 1 goes away: only the leave is reported, entity 5 stays seen.
        assert_eq!(
            vision.update(Vec3::ZERO, 0.0, &[seen(5, 10.0)], &occluder),
            vec![VisionEvent::StopSeeing(e(1))]
        );

        // Entity 5 follows: the set is empty again.
        assert_eq!(
            vision.update(Vec3::ZERO, 0.0, &[], &occluder),
            vec![VisionEvent::StopSeeing(e(5))]
        );
        assert!(vision.visible().is_empty());
    }

    /// Churn that fired between the last poll and the onload is consumed by
    /// the reconciliation instead of surfacing as callbacks.
    #[test]
    fn a_stale_leave_before_the_onload_is_not_a_callback() {
        let mut trap = ProximityTrap::new(1, 0, e(1), 10.0).expect("a positive range");
        trap.list_mut().add_entity(e(2), Position::new(1.0, 0.0));
        trap.list_mut().add_entity(e(4), Position::new(3.0, 0.0));
        trap.start();
        assert_eq!(
            trap.poll(),
            vec![TrapEvent::EnterTrap(e(2)), TrapEvent::EnterTrap(e(4))]
        );

        // e(2) wanders out of the box and back: the churn queues up and no
        // one polls it.
        trap.list_mut().move_entity(e(2), Position::new(50.0, 0.0));
        trap.list_mut().move_entity(e(2), Position::new(1.0, 0.0));

        // The onload reconciles the streamed set: both ids re-entered, so
        // the stale churn in the unseen window is consumed with no callback.
        let events = trap.start_onloaded(&[e(2), e(4)]);
        assert_eq!(events, &[]);
        assert!(trap.poll().is_empty());
    }
}
