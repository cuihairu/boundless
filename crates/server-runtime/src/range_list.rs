//! BW-08-03-01: port the range list and the trigger boundary.
//!
//! `RangeTrigger` (`range_trigger.cpp`) is a radius box anchored on one central
//! entity: `RangeTriggerNode::x()`/`z()` are `central.x() ± range_`
//! (`range_trigger.cpp:50`, `:68`), so the upper bound sits at `central + range`
//! and the lower bound at `central - range`. Membership is the half-open
//! interval of `isInXRange`/`isInZRange` (`range_trigger.hpp:110`, `:138`):
//! `subX - range < x && x <= subX + range` — the **upper edge is inside, the
//! lower edge is outside**. That asymmetry is the radius-critical detail.
//! An entity is triggered only when it is in the range of *both* axes; the
//! per-axis shuffle in `crossedXEntity` (`range_trigger.cpp:170`) and
//! `crossedZEntity` (`range_trigger.cpp:303`) fires `triggerEnter`/
//! `triggerLeave` (`range_trigger.hpp:86`) once per membership transition.
//! `RangeTrigger::setRange` (`range_trigger.cpp:632`) grows or shrinks the
//! radius, clamped to `0.001`; `insert` (`range_trigger.cpp:464`) sweeps the
//! bounds out from zero, firing `triggerEnter` for everything already inside;
//! `remove` (`range_trigger.cpp:541`) contracts to the minimum first, firing
//! `triggerLeave` for what it drops.
//!
//! The acceptance, "半径临界进入和离开事件吻合", is that at the radius boundary
//! the enter and leave events are exactly symmetric: the inclusive upper edge
//! is inside and the exclusive lower edge is outside, and crossing either
//! boundary in either axis fires exactly one enter or leave.

use std::collections::{BTreeMap, BTreeSet};

/// `EntityID` (`entity.hpp`), shared with the real entity.
pub use crate::real_entity::EntityId;

/// The floor `RangeTrigger::setRange` clamps a radius to
/// (`range_trigger.cpp:632`): `if (r < 0.001f) r = 0.001f;`.
pub const MIN_RANGE: f64 = 0.001;

/// A ground-plane position (x, z) — the two axes the range list is keyed on.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Position {
    /// The X coordinate.
    pub x: f64,
    /// The Z coordinate.
    pub z: f64,
}

impl Position {
    /// A position from an X and a Z coordinate.
    pub fn new(x: f64, z: f64) -> Self {
        Position { x, z }
    }

    /// The origin, `(0, 0)`.
    pub const ZERO: Position = Position { x: 0.0, z: 0.0 };
}

/// A trigger event, in the order it happened (`triggerEnter` /
/// `triggerLeave`, `range_trigger.hpp:86`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TriggerEvent {
    /// `RangeTrigger::triggerEnter`: the entity entered the radius.
    Enter(EntityId),
    /// `RangeTrigger::triggerLeave`: the entity left the radius.
    Leave(EntityId),
}

/// `RangeTrigger` (`range_trigger.cpp`): a radius box anchored on one central
/// entity. `contains` is the half-open box `(subject - range, subject + range]`
/// on each axis (`isInXRange`/`isInZRange`, `range_trigger.hpp:110`, `:138`),
/// so the upper edge is inside and the lower edge is outside. `transition`
/// recomputes one entity's membership against the recorded set and fires at
/// most one enter or leave — the net of the reference's per-axis shuffle.
#[derive(Debug, Clone)]
pub struct RangeTrigger {
    /// The central node's entity (`pCentralNode_`).
    subject: EntityId,
    /// The central node's position (`pCentralNode_->x()`/`z()`).
    subject_pos: Position,
    /// `range_`; clamped to [`MIN_RANGE`] on [`set_range`](Self::set_range).
    range: f64,
    /// The entities currently inside the box, excluding the subject.
    inside: BTreeSet<EntityId>,
}

impl RangeTrigger {
    /// A trigger on `subject` at `subject_pos` with radius `range`.
    pub fn new(subject: EntityId, subject_pos: Position, range: f64) -> Self {
        RangeTrigger {
            subject,
            subject_pos,
            range: if range < MIN_RANGE { MIN_RANGE } else { range },
            inside: BTreeSet::new(),
        }
    }

    /// The trigger's subject (`pCentralNode_`).
    pub fn subject(&self) -> EntityId {
        self.subject
    }

    /// The subject's position.
    pub fn subject_pos(&self) -> Position {
        self.subject_pos
    }

    /// The radius (`range_`).
    pub fn range(&self) -> f64 {
        self.range
    }

    /// Whether `pos` is inside this trigger's half-open box
    /// (`isInXRange`/`isInZRange`, `range_trigger.hpp:110`, `:138`).
    pub fn contains(&self, pos: Position) -> bool {
        let lx = self.subject_pos.x - self.range;
        let ux = self.subject_pos.x + self.range;
        let lz = self.subject_pos.z - self.range;
        let uz = self.subject_pos.z + self.range;
        // Half-open: `subX - range < x && x <= subX + range` on each axis.
        (lx < pos.x && pos.x <= ux) && (lz < pos.z && pos.z <= uz)
    }

    /// The entities currently inside the box.
    pub fn inside(&self) -> &BTreeSet<EntityId> {
        &self.inside
    }

    /// Whether the entity is inside the box (the subject is never inside).
    pub fn is_inside(&self, entity: EntityId) -> bool {
        entity != self.subject && self.inside.contains(&entity)
    }

    /// `RangeTrigger::setRange` (`range_trigger.cpp:632`): change the radius,
    /// clamped to [`MIN_RANGE`]. The caller re-sweeps the entities so
    /// expansion fires enter and contraction fires leave.
    pub fn set_range(&mut self, range: f64) {
        self.range = if range < MIN_RANGE { MIN_RANGE } else { range };
    }

    /// Move the central node (`RangeTrigger::shuffleXThenZ`,
    /// `range_trigger.cpp:570`). The caller re-sweeps the entities against the
    /// new anchor.
    pub fn set_subject_pos(&mut self, pos: Position) {
        self.subject_pos = pos;
    }

    /// Recompute one entity's membership and return the enter or leave it
    /// crossed (`crossedXEntity`/`crossedZEntity`, `range_trigger.cpp:170`,
    /// `:303`), or `None` when nothing changed. The subject is skipped
    /// (`range_trigger.cpp:170`: `if (pNode == pRange_->pCentralNode())`).
    pub fn transition(&mut self, entity: EntityId, pos: Position) -> Option<TriggerEvent> {
        if entity == self.subject {
            return None;
        }
        let was = self.inside.contains(&entity);
        let is = self.contains(pos);
        if was == is {
            return None;
        }
        if is {
            self.inside.insert(entity);
            Some(TriggerEvent::Enter(entity))
        } else {
            self.inside.remove(&entity);
            Some(TriggerEvent::Leave(entity))
        }
    }

    /// Drop an entity from the box without an event (the trigger is being torn
    /// down, or the entity left the space).
    pub fn forget(&mut self, entity: EntityId) {
        self.inside.remove(&entity);
    }
}

/// `RangeList` (`range_list.cpp`): the entities of a space keyed on X then Z,
/// driving their `RangeTrigger`s. Every change — an entity moving, a trigger's
/// radius growing or shrinking, a trigger's anchor moving, an entity or trigger
/// arriving or leaving — re-sweeps the affected triggers and records the enter
/// and leave events in order.
#[derive(Debug, Default)]
pub struct RangeList {
    /// The entities of the space.
    entities: BTreeMap<EntityId, Position>,
    /// The triggers, keyed by their subject.
    triggers: BTreeMap<EntityId, RangeTrigger>,
    /// Every trigger event, in the order it happened.
    events: Vec<TriggerEvent>,
}

impl RangeList {
    /// An empty range list.
    pub fn new() -> Self {
        RangeList::default()
    }

    /// The entities of the space.
    pub fn entities(&self) -> &BTreeMap<EntityId, Position> {
        &self.entities
    }

    /// The triggers, keyed by subject.
    pub fn triggers(&self) -> &BTreeMap<EntityId, RangeTrigger> {
        &self.triggers
    }

    /// Every trigger event, in the order it happened.
    pub fn events(&self) -> &[TriggerEvent] {
        &self.events
    }

    /// The trigger on `subject`, if any.
    pub fn trigger(&self, subject: EntityId) -> Option<&RangeTrigger> {
        self.triggers.get(&subject)
    }

    /// The entities currently inside `subject`'s trigger.
    pub fn inside(&self, subject: EntityId) -> Option<&BTreeSet<EntityId>> {
        self.triggers.get(&subject).map(RangeTrigger::inside)
    }

    /// Bring an entity into the space at `pos`; entities already inside a
    /// trigger's box fire `triggerEnter` (`RangeTrigger::insert`,
    /// `range_trigger.cpp:464`). A repeat of an existing entity moves it.
    pub fn add_entity(&mut self, id: EntityId, pos: Position) {
        if self.entities.contains_key(&id) {
            self.move_entity(id, pos);
            return;
        }
        self.entities.insert(id, pos);
        for trigger in self.triggers.values_mut() {
            if trigger.subject() != id
                && let Some(event) = trigger.transition(id, pos)
            {
                self.events.push(event);
            }
        }
    }

    /// Take an entity out of the space; anything inside its triggers fires
    /// `triggerLeave` (`RangeTrigger::remove`, `range_trigger.cpp:541`).
    pub fn remove_entity(&mut self, id: EntityId) {
        if self.entities.remove(&id).is_none() {
            return;
        }
        for trigger in self.triggers.values_mut() {
            if trigger.is_inside(id) {
                trigger.forget(id);
                self.events.push(TriggerEvent::Leave(id));
            }
        }
    }

    /// Move an entity to `new_pos`, firing enter or leave on every trigger it
    /// crossed the radius boundary of (`crossedXEntity`/`crossedZEntity`).
    /// When the moved entity *is* a trigger's subject, its box moves with it
    /// (`RangeTrigger::shuffleXThenZ`, `range_trigger.cpp:570`) and every other
    /// entity is re-swept against the new anchor.
    pub fn move_entity(&mut self, id: EntityId, new_pos: Position) {
        if !self.entities.contains_key(&id) {
            return;
        }
        self.entities.insert(id, new_pos);
        let triggers: Vec<EntityId> = self.triggers.keys().copied().collect();
        for subject in triggers {
            let trigger = self.triggers.get_mut(&subject).expect("trigger exists");
            if subject == id {
                trigger.set_subject_pos(new_pos);
                for (other, other_pos) in &self.entities {
                    if *other != id
                        && let Some(event) = trigger.transition(*other, *other_pos)
                    {
                        self.events.push(event);
                    }
                }
            } else if let Some(event) = trigger.transition(id, new_pos) {
                self.events.push(event);
            }
        }
    }

    /// `RangeTrigger::insert` (`range_trigger.cpp:464`): install a trigger on
    /// `subject`; entities already inside its box fire `triggerEnter`.
    pub fn add_trigger(&mut self, subject: EntityId, range: f64) {
        let subject_pos = self
            .entities
            .get(&subject)
            .copied()
            .unwrap_or(Position::ZERO);
        let mut trigger = RangeTrigger::new(subject, subject_pos, range);
        for (id, pos) in &self.entities {
            if *id != subject
                && let Some(event) = trigger.transition(*id, *pos)
            {
                self.events.push(event);
            }
        }
        self.triggers.insert(subject, trigger);
    }

    /// `RangeTrigger::setRange` (`range_trigger.cpp:632`): change the radius,
    /// firing enter for entities the box grew over and leave for ones it
    /// shrank away.
    pub fn set_trigger_range(&mut self, subject: EntityId, range: f64) {
        let Some(trigger) = self.triggers.get_mut(&subject) else {
            return;
        };
        trigger.set_range(range);
        for (id, pos) in &self.entities {
            if *id != subject
                && let Some(event) = trigger.transition(*id, *pos)
            {
                self.events.push(event);
            }
        }
    }

    /// `RangeTrigger::remove` (`range_trigger.cpp:541`): contract to the
    /// minimum first, firing `triggerLeave` for what it drops, then tear the
    /// trigger down.
    pub fn remove_trigger(&mut self, subject: EntityId) {
        if let Some(trigger) = self.triggers.remove(&subject) {
            for id in trigger.inside() {
                self.events.push(TriggerEvent::Leave(*id));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn enters(events: &[TriggerEvent]) -> Vec<EntityId> {
        events
            .iter()
            .filter_map(|e| match e {
                TriggerEvent::Enter(id) => Some(*id),
                _ => None,
            })
            .collect()
    }

    fn leaves(events: &[TriggerEvent]) -> Vec<EntityId> {
        events
            .iter()
            .filter_map(|e| match e {
                TriggerEvent::Leave(id) => Some(*id),
                _ => None,
            })
            .collect()
    }

    /// A trigger at the origin with radius 10, with entity 2 at `pos`.
    fn with_trigger(pos: Position) -> RangeList {
        let mut list = RangeList::new();
        list.add_entity(1, Position::ZERO); // the subject
        list.add_entity(2, pos);
        list.add_trigger(1, 10.0);
        list
    }

    /// The acceptance, first half: an entity crossing the radius boundary
    /// inward fires exactly one enter, and stays inside on further moves.
    #[test]
    fn an_entity_crossing_the_radius_inward_fires_one_enter() {
        let mut list = with_trigger(Position::new(15.0, 0.0));
        assert!(list.trigger(1).unwrap().inside().is_empty());

        list.move_entity(2, Position::new(5.0, 0.0));

        assert_eq!(enters(list.events()), vec![2]);
        assert_eq!(list.trigger(1).unwrap().inside().len(), 1);
        assert!(list.trigger(1).unwrap().is_inside(2));
    }

    /// The acceptance, second half: an entity crossing the radius boundary
    /// outward fires exactly one leave, and stays out on further moves.
    #[test]
    fn an_entity_crossing_the_radius_outward_fires_one_leave() {
        let mut list = with_trigger(Position::new(5.0, 0.0));
        assert!(list.trigger(1).unwrap().is_inside(2));

        list.move_entity(2, Position::new(15.0, 0.0));

        assert_eq!(leaves(list.events()), vec![2]);
        assert!(list.trigger(1).unwrap().inside().is_empty());
    }

    /// A move that stays inside the radius fires nothing new — the only event
    /// is the enter from when the trigger was installed over the entity.
    #[test]
    fn a_move_that_stays_inside_fires_nothing() {
        let mut list = with_trigger(Position::new(5.0, 0.0));
        list.move_entity(2, Position::new(6.0, 0.0));
        list.move_entity(2, Position::new(4.0, 0.0));
        assert_eq!(list.events(), &[TriggerEvent::Enter(2)]);
    }

    /// A move that stays outside the radius fires nothing, even when it crosses
    /// one axis's line while the other axis stays outside
    /// (`crossedXEntity`: `if (!wasInZ) return;`, `range_trigger.cpp:170`).
    #[test]
    fn a_move_that_stays_outside_on_one_axis_fires_nothing() {
        let mut list = with_trigger(Position::new(-5.0, 20.0));
        assert!(list.trigger(1).unwrap().inside().is_empty());

        // Cross the X band while Z is far outside.
        list.move_entity(2, Position::new(5.0, 20.0));

        assert_eq!(list.events(), &[]);
    }

    /// Radius-critical: the upper edge `subject + range` is **inside**
    /// (`x <= subX + range`), the lower edge `subject - range` is **outside**
    /// (`subX - range < x`) — `isInXRange`, `range_trigger.hpp:110`.
    #[test]
    fn the_upper_edge_is_inside_and_the_lower_edge_is_outside() {
        let mut list = RangeList::new();
        list.add_entity(1, Position::ZERO);
        list.add_trigger(1, 10.0);

        // Upper edge on the +X side: `10 <= 0 + 10` holds.
        list.add_entity(2, Position::new(10.0, 0.0));
        assert!(list.trigger(1).unwrap().is_inside(2), "upper edge is in");

        // Lower edge on the -X side: `-10 < 0 - 10` fails, so it is out.
        list.add_entity(3, Position::new(-10.0, 0.0));
        assert!(!list.trigger(1).unwrap().is_inside(3), "lower edge is out");

        // A hair inside the lower edge is in.
        list.add_entity(4, Position::new(-9.5, 0.0));
        assert!(list.trigger(1).unwrap().is_inside(4));
    }

    /// Radius-critical: entering and leaving at the upper edge are symmetric —
    /// exactly one enter on the way in, exactly one leave on the way out.
    #[test]
    fn crossing_the_upper_edge_fires_symmetric_enter_and_leave() {
        let mut list = RangeList::new();
        list.add_entity(1, Position::ZERO);
        list.add_entity(2, Position::new(11.0, 0.0)); // just outside
        list.add_trigger(1, 10.0);

        // Inward to exactly the upper edge.
        list.move_entity(2, Position::new(10.0, 0.0));
        assert_eq!(enters(list.events()), vec![2]);

        // Outward past the upper edge.
        list.move_entity(2, Position::new(11.0, 0.0));
        assert_eq!(leaves(list.events()), vec![2]);
        assert_eq!(list.events().len(), 2, "exactly one enter and one leave");
    }

    /// Radius-critical: entering and leaving at the lower edge are symmetric
    /// the other way — `subject - range` is the first position *outside*.
    #[test]
    fn crossing_the_lower_edge_fires_symmetric_enter_and_leave() {
        let mut list = RangeList::new();
        list.add_entity(1, Position::ZERO);
        list.add_entity(2, Position::new(-11.0, 0.0)); // just outside
        list.add_trigger(1, 10.0);

        // Inward to just inside the lower edge (not onto it).
        list.move_entity(2, Position::new(-9.5, 0.0));
        assert_eq!(enters(list.events()), vec![2]);
        assert!(list.trigger(1).unwrap().is_inside(2));

        // Outward back to just outside.
        list.move_entity(2, Position::new(-11.0, 0.0));
        assert_eq!(leaves(list.events()), vec![2]);
        assert_eq!(list.events().len(), 2);
    }

    /// An entity exactly on the lower edge is outside, so arriving there from
    /// outside fires nothing and departing from inside fires leave.
    #[test]
    fn the_lower_edge_itself_fires_no_enter() {
        let mut list = RangeList::new();
        list.add_entity(1, Position::ZERO);
        list.add_entity(2, Position::new(-11.0, 0.0));
        list.add_trigger(1, 10.0);

        // Step from -11 to exactly -10: still outside the half-open band.
        list.move_entity(2, Position::new(-10.0, 0.0));
        assert_eq!(list.events(), &[]);
        assert!(!list.trigger(1).unwrap().is_inside(2));
    }

    /// A diagonal corner crossing fires exactly one net event per transition:
    /// the enter from installation and one leave.
    #[test]
    fn a_diagonal_corner_exit_fires_one_leave() {
        let mut list = with_trigger(Position::new(9.0, 9.0));
        assert!(list.trigger(1).unwrap().is_inside(2));

        list.move_entity(2, Position::new(11.0, 11.0));

        assert_eq!(
            list.events(),
            &[TriggerEvent::Enter(2), TriggerEvent::Leave(2)],
            "one net leave for a corner crossing"
        );
    }

    /// `RangeTrigger::setRange` (`range_trigger.cpp:632`): growing the radius
    /// fires enter for the entity it grew over.
    #[test]
    fn growing_the_radius_fires_enter() {
        let mut list = RangeList::new();
        list.add_entity(1, Position::ZERO);
        list.add_trigger(1, 5.0);
        list.add_entity(2, Position::new(7.0, 0.0));
        assert!(list.trigger(1).unwrap().inside().is_empty(), "outside 5");

        list.set_trigger_range(1, 10.0);

        assert_eq!(enters(list.events()), vec![2]);
        assert!(list.trigger(1).unwrap().is_inside(2));
    }

    /// `RangeTrigger::setRange`: shrinking the radius fires leave for the
    /// entity it shrank away.
    #[test]
    fn shrinking_the_radius_fires_leave() {
        let mut list = with_trigger(Position::new(7.0, 0.0));
        assert!(list.trigger(1).unwrap().is_inside(2));

        list.set_trigger_range(1, 5.0);

        assert_eq!(leaves(list.events()), vec![2]);
        assert!(list.trigger(1).unwrap().inside().is_empty());
    }

    /// `RangeTrigger::setRange` clamps the radius to `0.001`
    /// (`range_trigger.cpp:632`).
    #[test]
    fn the_radius_is_clamped_to_the_minimum() {
        let mut list = with_trigger(Position::new(0.0005, 0.0));
        assert!(list.trigger(1).unwrap().is_inside(2));

        list.set_trigger_range(1, 0.0);

        assert_eq!(list.trigger(1).unwrap().range(), MIN_RANGE);
        // 0.0005 < 0.001, so the entity stays inside after the clamp and no
        // leave fires — the only event is the enter from installation.
        assert_eq!(list.events(), &[TriggerEvent::Enter(2)]);
        assert!(list.trigger(1).unwrap().is_inside(2));
    }

    /// Moving a trigger's subject sweeps the box: an entity that the box
    /// grew over fires enter, and one it moved away from fires leave
    /// (`RangeTrigger::shuffleXThenZ`, `range_trigger.cpp:570`).
    #[test]
    fn moving_a_trigger_subject_sweeps_the_box() {
        let mut list = with_trigger(Position::new(12.0, 0.0));
        assert!(list.trigger(1).unwrap().inside().is_empty());

        // The subject walks toward the entity: it enters its own box.
        list.move_entity(1, Position::new(3.0, 0.0));
        assert_eq!(enters(list.events()), vec![2]);
        assert!(list.trigger(1).unwrap().is_inside(2));

        // The subject walks away: the entity leaves.
        list.move_entity(1, Position::ZERO);
        assert_eq!(leaves(list.events()), vec![2]);
    }

    /// The subject is never triggered by its own trigger
    /// (`range_trigger.cpp:170`: `if (pNode == pRange_->pCentralNode())`) —
    /// the only event is the enter from installation.
    #[test]
    fn the_subject_itself_is_never_triggered() {
        let mut list = with_trigger(Position::new(5.0, 0.0));
        list.move_entity(1, Position::new(10.0, 0.0)); // the subject moves
        list.move_entity(1, Position::ZERO);
        assert_eq!(list.events(), &[TriggerEvent::Enter(2)]);
        assert!(!list.trigger(1).unwrap().is_inside(1));
    }

    /// An entity added inside an existing trigger's box fires enter.
    #[test]
    fn adding_an_entity_inside_fires_enter() {
        let mut list = RangeList::new();
        list.add_entity(1, Position::ZERO);
        list.add_trigger(1, 10.0);

        list.add_entity(2, Position::new(5.0, 0.0));

        assert_eq!(enters(list.events()), vec![2]);
    }

    /// An entity removed from inside a trigger's box fires leave.
    #[test]
    fn removing_an_entity_inside_fires_leave() {
        let mut list = with_trigger(Position::new(5.0, 0.0));
        assert!(list.trigger(1).unwrap().is_inside(2));

        list.remove_entity(2);

        assert_eq!(leaves(list.events()), vec![2]);
        assert!(list.trigger(1).unwrap().inside().is_empty());
    }

    /// `RangeTrigger::insert` (`range_trigger.cpp:464`): a trigger added when
    /// an entity is already inside fires enter for it.
    #[test]
    fn adding_a_trigger_fires_enter_for_entities_already_inside() {
        let mut list = RangeList::new();
        list.add_entity(1, Position::ZERO);
        list.add_entity(2, Position::new(5.0, 0.0));

        list.add_trigger(1, 10.0);

        assert_eq!(enters(list.events()), vec![2]);
    }

    /// `RangeTrigger::remove` (`range_trigger.cpp:541`): removing a trigger
    /// fires leave for everything it still holds.
    #[test]
    fn removing_a_trigger_fires_leave_for_all_inside() {
        let mut list = RangeList::new();
        list.add_entity(1, Position::ZERO);
        list.add_entity(2, Position::new(5.0, 0.0));
        list.add_entity(3, Position::new(6.0, 0.0));
        list.add_trigger(1, 10.0);
        assert_eq!(list.trigger(1).unwrap().inside().len(), 2);

        list.remove_trigger(1);

        assert_eq!(leaves(list.events()), vec![2, 3]);
        assert!(list.trigger(1).is_none());
    }
}
