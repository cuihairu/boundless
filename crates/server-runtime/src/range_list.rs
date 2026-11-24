//! BW-08-03-01: the range list and its crossing triggers.
//!
//! Ported from the reference cell app faces:
//! `server/cellapp/range_list_node.{hpp,cpp}` (the sorted node lists and the
//! shuffle algorithm), `server/cellapp/range_trigger.{hpp,cpp}` (the
//! upper/lower bound pairs that fire enter/leave events),
//! `server/cellapp/cell_range_list.{hpp,cpp,ipp}` (the `RangeList`
//! container), `server/cellapp/entity_range_list_node.{hpp,cpp}` (the entity
//! node) and `server/cellapp/range_list_appeal_trigger.{hpp,cpp}` plus the
//! `AoITrigger` in `server/cellapp/witness.cpp:73` (the two trigger flavours).
//!
//! Layout: the C++ version threads intrusive `pPrevX_/pNextX_/pPrevZ_/pNextZ_`
//! pointers through heap nodes. Here every node lives in a `Vec` slot and the
//! links are slot indices, which keeps the shuffle algorithm line-for-line
//! while staying in safe Rust. `isEqual(a, b)` in the reference
//! (`lib/cstdmf/stdmf_minimal.hpp:119`) is exact float equality
//! (`fabsf(a-b) <= 0.f`), so the shuffle tie-breakers use `==`.
//!
//! Crossing callbacks become an event log: `triggerEnter`/`triggerLeave`
//! (`range_trigger.hpp:86-87`) push [`RangeEvent`] values that the witness
//! (BW-08-03-02) drains, which makes the enter/leave sequence directly
//! observable.

use crate::entity_creator::EntityId;
use foundation::math::Vector3;

/// Node/trigger arena slot index. Slots are stable for the life of the list.
pub type NodeId = usize;

/// Trigger arena slot index.
pub type TriggerId = usize;

/// `RangeListOrder` (`range_list_node.hpp:18-25`): tie-breaker for nodes at
/// equal coordinates. Within one axis list, equal-coordinate nodes keep
/// ascending order.
pub mod order {
    /// `RANGE_LIST_ORDER_HEAD`.
    pub const HEAD: u16 = 0;
    /// `RANGE_LIST_ORDER_ENTITY`.
    pub const ENTITY: u16 = 100;
    /// `RANGE_LIST_ORDER_LOWER_BOUND`.
    pub const LOWER_BOUND: u16 = 190;
    /// `RANGE_LIST_ORDER_UPPER_BOUND`.
    pub const UPPER_BOUND: u16 = 200;
    /// `RANGE_LIST_ORDER_TAIL`.
    pub const TAIL: u16 = 0xffff;
}

/// `RangeListNode::RangeListFlags` (`range_list_node.hpp:37-46`). What
/// crossings a node wants from others (`wants`) and what it announces to
/// others (`makes`); a crossing is dispatched exactly when
/// `wants & other.makes != 0`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Flags(pub u16);

impl Flags {
    /// `FLAG_NO_TRIGGERS`.
    pub const NO_TRIGGERS: Flags = Flags(0);
    /// `FLAG_ENTITY_TRIGGER`.
    pub const ENTITY_TRIGGER: Flags = Flags(0x01);
    /// `FLAG_LOWER_AOI_TRIGGER`.
    pub const LOWER_AOI_TRIGGER: Flags = Flags(0x02);
    /// `FLAG_UPPER_AOI_TRIGGER`.
    pub const UPPER_AOI_TRIGGER: Flags = Flags(0x04);
    /// `FLAG_IS_ENTITY`.
    pub const IS_ENTITY: Flags = Flags(0x10);
    /// `FLAG_IS_LOWER_BOUND`.
    pub const IS_LOWER_BOUND: Flags = Flags(0x20);

    /// `wantsCrossingWith`: `wantsFlags_ & pOther->makesFlags_`.
    pub fn wants_crossing_with(self, other_makes: Flags) -> bool {
        (self.0 & other_makes.0) != 0
    }

    /// Bitwise union, used when composing `makes` sets.
    pub fn union(self, other: Flags) -> Flags {
        Flags(self.0 | other.0)
    }

    /// Bitwise intersection test used by the shuffle predicates.
    pub fn contains(self, other: Flags) -> bool {
        (self.0 & other.0) == other.0
    }
}

/// What a node carries. Mirrors the C++ class hierarchy: `Entity` (an
/// `EntityRangeListNode`), `Trigger` (a `RangeTriggerNode` bound) and
/// `Terminator` (a `RangeListTerminator`).
#[derive(Debug, Clone, Copy)]
enum NodeKind {
    Entity {
        id: EntityId,
        pos: Vector3,
        /// Set when the node has been unlinked by [`RangeList::remove_entity`];
        /// the slot stays reserved so indices remain stable.
        removed: bool,
    },
    Trigger {
        owner: TriggerId,
        /// Signed: the lower bound carries a negative range
        /// (`range_trigger.cpp:441`).
        range: f32,
        old_range: f32,
    },
    Terminator {
        head: bool,
    },
}

/// One node of the doubly linked X/Z lists (`range_list_node.hpp:34-125`).
#[derive(Debug, Clone, Copy)]
struct Node {
    prev_x: Option<NodeId>,
    next_x: Option<NodeId>,
    prev_z: Option<NodeId>,
    next_z: Option<NodeId>,
    wants: Flags,
    makes: Flags,
    order: u16,
    kind: NodeKind,
}

impl Node {
    /// `isEntity()` (`range_list_node.hpp:86`).
    fn is_entity(&self) -> bool {
        self.makes.contains(Flags::IS_ENTITY)
    }

    /// `isLowerBound()` (`range_list_node.hpp:87`).
    fn is_lower_bound(&self) -> bool {
        self.makes.contains(Flags::IS_LOWER_BOUND)
    }
}

/// Per-trigger bookkeeping (`RangeTrigger` fields,
/// `range_trigger.hpp:219-228`).
#[derive(Debug, Clone, Copy)]
struct TriggerData {
    /// `pCentralNode_`: the entity node the bounds are offset from.
    central: NodeId,
    /// Lower bound node.
    lower: NodeId,
    /// Upper bound node.
    upper: NodeId,
    /// `oldEntityX_`.
    old_entity_x: f32,
    /// `oldEntityZ_`.
    old_entity_z: f32,
}

/// Which direction a trigger event went.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventKind {
    /// `RangeTrigger::triggerEnter`.
    Enter,
    /// `RangeTrigger::triggerLeave`.
    Leave,
}

/// One `triggerEnter`/`triggerLeave` firing (`range_trigger.hpp:86-87`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RangeEvent {
    /// The trigger that fired.
    pub trigger: TriggerId,
    /// The entity that entered or left.
    pub entity: EntityId,
    /// Enter or leave.
    pub kind: EventKind,
}

/// The range list: two doubly linked coordinate-sorted node lists (X and Z)
/// sharing one head/tail pair, plus the trigger bookkeeping
/// (`cell_range_list.hpp:12-32`).
pub struct RangeList {
    nodes: Vec<Node>,
    triggers: Vec<TriggerData>,
    /// Fired enter/leave events, drained by [`RangeList::drain_events`].
    events: Vec<RangeEvent>,
    /// Head terminator, at `-FLT_MAX` (`cell_range_list.cpp:26`).
    head: NodeId,
    /// Tail terminator, at `+FLT_MAX`.
    tail: NodeId,
}

/// The reference `isEqual(float, float)` is exact equality
/// (`stdmf_minimal.hpp:119-122`).
fn is_equal(a: f32, b: f32) -> bool {
    a == b
}

impl RangeList {
    /// `RangeList::RangeList` (`cell_range_list.cpp:26-30`): head and tail
    /// terminators, tail linked before head so an empty list is
    /// `head -> tail` on both axes.
    pub fn new() -> Self {
        let mut nodes = Vec::new();
        let head = nodes.len();
        nodes.push(Node {
            prev_x: None,
            next_x: None,
            prev_z: None,
            next_z: None,
            wants: Flags::NO_TRIGGERS,
            makes: Flags::NO_TRIGGERS,
            order: order::HEAD,
            kind: NodeKind::Terminator { head: true },
        });
        let tail = nodes.len();
        nodes.push(Node {
            prev_x: None,
            next_x: None,
            prev_z: None,
            next_z: None,
            wants: Flags::NO_TRIGGERS,
            makes: Flags::NO_TRIGGERS,
            order: order::TAIL,
            kind: NodeKind::Terminator { head: false },
        });
        let mut list = RangeList {
            nodes,
            triggers: Vec::new(),
            events: Vec::new(),
            head,
            tail,
        };
        // last_.insertBeforeX( &first_ ); last_.insertBeforeZ( &first_ );
        list.insert_before_x(tail, head);
        list.insert_before_z(tail, head);
        list
    }

    // ---- coordinate accessors -------------------------------------------

    /// `RangeListNode::x()` dispatch. Terminators sit at `+-FLT_MAX`
    /// (`range_list_node.hpp:138-139`).
    fn x(&self, node: NodeId) -> f32 {
        match self.nodes[node].kind {
            NodeKind::Entity { pos, .. } => pos.x,
            NodeKind::Trigger { owner, range, .. } => self.x(self.triggers[owner].central) + range,
            NodeKind::Terminator { head } => {
                if head {
                    -f32::MAX
                } else {
                    f32::MAX
                }
            }
        }
    }

    /// `RangeListNode::z()` dispatch.
    fn z(&self, node: NodeId) -> f32 {
        match self.nodes[node].kind {
            NodeKind::Entity { pos, .. } => pos.z,
            NodeKind::Trigger { owner, range, .. } => self.z(self.triggers[owner].central) + range,
            NodeKind::Terminator { head } => {
                if head {
                    -f32::MAX
                } else {
                    f32::MAX
                }
            }
        }
    }

    /// The owning trigger of a bound node (bounds are the only nodes that
    /// ever want crossings, so the dispatch guarantees this is a bound).
    fn bound_owner(&self, node: NodeId) -> TriggerId {
        match self.nodes[node].kind {
            NodeKind::Trigger { owner, .. } => owner,
            _ => 0,
        }
    }

    /// A trigger bound node's current signed range.
    fn node_range(&self, node: NodeId) -> f32 {
        match self.nodes[node].kind {
            NodeKind::Trigger { range, .. } => range,
            _ => 0.0,
        }
    }

    /// A trigger bound node's `oldRange_`.
    fn node_old_range(&self, node: NodeId) -> f32 {
        match self.nodes[node].kind {
            NodeKind::Trigger { old_range, .. } => old_range,
            _ => 0.0,
        }
    }

    /// A bound node's `oldX()`: old entity position plus old range
    /// (`range_trigger.cpp:86-95`).
    fn bound_old_x(&self, node: NodeId) -> f32 {
        match self.nodes[node].kind {
            NodeKind::Trigger {
                owner, old_range, ..
            } => self.triggers[owner].old_entity_x + old_range,
            _ => self.x(node),
        }
    }

    /// A bound node's `oldZ()` (`range_trigger.cpp:104-113`).
    fn bound_old_z(&self, node: NodeId) -> f32 {
        match self.nodes[node].kind {
            NodeKind::Trigger {
                owner, old_range, ..
            } => self.triggers[owner].old_entity_z + old_range,
            _ => self.z(node),
        }
    }

    // ---- link surgery (private) ------------------------------------------

    /// `insertBeforeX` (`range_list_node.cpp:246-259`).
    fn insert_before_x(&mut self, anchor: NodeId, node: NodeId) {
        let prev = self.nodes[anchor].prev_x;
        if let Some(p) = prev {
            self.nodes[p].next_x = Some(node);
        }
        self.nodes[node].prev_x = prev;
        self.nodes[anchor].prev_x = Some(node);
        self.nodes[node].next_x = Some(anchor);
    }

    /// `insertBeforeZ` (`range_list_node.cpp:267-280`).
    fn insert_before_z(&mut self, anchor: NodeId, node: NodeId) {
        let prev = self.nodes[anchor].prev_z;
        if let Some(p) = prev {
            self.nodes[p].next_z = Some(node);
        }
        self.nodes[node].prev_z = prev;
        self.nodes[anchor].prev_z = Some(node);
        self.nodes[node].next_z = Some(anchor);
    }

    /// `removeFromRangeList` (`range_list_node.cpp:226-238`).
    fn unlink(&mut self, node: NodeId) {
        if let Some(p) = self.nodes[node].prev_x {
            self.nodes[p].next_x = self.nodes[node].next_x;
        }
        if let Some(n) = self.nodes[node].next_x {
            self.nodes[n].prev_x = self.nodes[node].prev_x;
        }
        if let Some(p) = self.nodes[node].prev_z {
            self.nodes[p].next_z = self.nodes[node].next_z;
        }
        if let Some(n) = self.nodes[node].next_z {
            self.nodes[n].prev_z = self.nodes[node].prev_z;
        }
        self.nodes[node].prev_x = None;
        self.nodes[node].next_x = None;
        self.nodes[node].prev_z = None;
        self.nodes[node].next_z = None;
    }

    // ---- crossing dispatch -----------------------------------------------

    /// `RangeTriggerNode::crossedX` (`range_trigger.cpp:146-157`): route to
    /// the entity or the trigger-range handler. Only bound nodes ever want
    /// crossings, so `node` is always a bound here.
    fn crossed_x(
        &mut self,
        node: NodeId,
        other: NodeId,
        positive: bool,
        old_oth_x: f32,
        old_oth_z: f32,
    ) {
        if self.nodes[other].is_entity() {
            self.crossed_x_entity(node, other, positive, old_oth_x, old_oth_z);
        } else {
            self.crossed_x_entity_range(node, other, positive);
        }
    }

    /// `RangeTriggerNode::crossedZ` (`range_trigger.cpp:279-290`).
    fn crossed_z(
        &mut self,
        node: NodeId,
        other: NodeId,
        positive: bool,
        old_oth_x: f32,
        old_oth_z: f32,
    ) {
        if self.nodes[other].is_entity() {
            self.crossed_z_entity(node, other, positive, old_oth_x, old_oth_z);
        } else {
            self.crossed_z_entity_range(node, other, positive);
        }
    }

    /// `RangeTriggerNode::crossedXEntity` (`range_trigger.cpp:170-206`).
    /// X is shuffled first, so the other node's OLD z decides whether this
    /// crossing is relevant.
    fn crossed_x_entity(
        &mut self,
        node: NodeId,
        other: NodeId,
        positive: bool,
        old_oth_x: f32,
        old_oth_z: f32,
    ) {
        let owner = self.bound_owner(node);
        if other == self.triggers[owner].central {
            return;
        }
        let abs_old_range = self.node_old_range(node).abs();
        let was_in_z = self.was_in_z_range(owner, old_oth_z, abs_old_range);
        if !was_in_z {
            return;
        }
        let entity = self.entity_id(other);
        let is_entering = self.nodes[node].is_lower_bound() == positive;
        if is_entering {
            let abs_range = self.node_range(node).abs();
            let is_in_x = self.is_in_x_range(owner, self.x(other), abs_range);
            let is_in_z = self.is_in_z_range(owner, self.z(other), abs_range);
            if is_in_x && is_in_z {
                self.push_event(owner, entity, EventKind::Enter);
            }
        } else {
            let was_in_x = self.was_in_x_range(owner, old_oth_x, abs_old_range);
            if was_in_x {
                self.push_event(owner, entity, EventKind::Leave);
            }
        }
    }

    /// `RangeTriggerNode::crossedZEntity` (`range_trigger.cpp:303-339`).
    /// Z is shuffled second, so the other node's NEW x position is checked.
    fn crossed_z_entity(
        &mut self,
        node: NodeId,
        other: NodeId,
        positive: bool,
        old_oth_x: f32,
        old_oth_z: f32,
    ) {
        let owner = self.bound_owner(node);
        if other == self.triggers[owner].central {
            return;
        }
        let abs_range = self.node_range(node).abs();
        let is_in_x = self.is_in_x_range(owner, self.x(other), abs_range);
        if !is_in_x {
            return;
        }
        let entity = self.entity_id(other);
        let is_entering = self.nodes[node].is_lower_bound() == positive;
        if is_entering {
            let is_in_z = self.is_in_z_range(owner, self.z(other), abs_range);
            if is_in_z {
                self.push_event(owner, entity, EventKind::Enter);
            }
        } else {
            let abs_old_range = self.node_old_range(node).abs();
            let was_in_x = self.was_in_x_range(owner, old_oth_x, abs_old_range);
            let was_in_z = self.was_in_z_range(owner, old_oth_z, abs_old_range);
            if was_in_x && was_in_z {
                self.push_event(owner, entity, EventKind::Leave);
            }
        }
    }

    /// `RangeTriggerNode::crossedXEntityRange` (`range_trigger.cpp:213-266`):
    /// this bound crossed another trigger's bounds (the large-entity appeal
    /// path). The extent checks use the three-argument overlap forms
    /// (`range_trigger.hpp:154-207`): `(center - range <= otherUpper) &&
    /// (otherLower <= center + range)`.
    fn crossed_x_entity_range(&mut self, node: NodeId, other: NodeId, positive: bool) {
        let (owner, other_owner) = (self.bound_owner(node), self.bound_owner(other));
        let other_lower = self.triggers[other_owner].lower;
        let other_upper = self.triggers[other_owner].upper;
        let abs_old_range = self.node_old_range(node).abs();
        // x is shuffled first, so the other trigger's old z extent is checked.
        let was_in_z = self.was_in_z_extent(
            owner,
            self.bound_old_z(other_lower),
            self.bound_old_z(other_upper),
            abs_old_range,
        );
        if !was_in_z {
            return;
        }
        let entity = self.trigger_entity(other_owner);
        let is_entering = self.nodes[node].is_lower_bound() == positive;
        if is_entering {
            let abs_range = self.node_range(node).abs();
            let is_in_x =
                self.is_in_x_extent(owner, self.x(other_lower), self.x(other_upper), abs_range);
            let is_in_z =
                self.is_in_z_extent(owner, self.z(other_lower), self.z(other_upper), abs_range);
            if is_in_x && is_in_z {
                self.push_event(owner, entity, EventKind::Enter);
            }
        } else {
            let was_in_x = self.was_in_x_extent(
                owner,
                self.bound_old_x(other_lower),
                self.bound_old_x(other_upper),
                abs_old_range,
            );
            if was_in_x {
                self.push_event(owner, entity, EventKind::Leave);
            }
        }
    }

    /// `RangeTriggerNode::crossedZEntityRange` (`range_trigger.cpp:346-399`).
    fn crossed_z_entity_range(&mut self, node: NodeId, other: NodeId, positive: bool) {
        let (owner, other_owner) = (self.bound_owner(node), self.bound_owner(other));
        let other_lower = self.triggers[other_owner].lower;
        let other_upper = self.triggers[other_owner].upper;
        let abs_range = self.node_range(node).abs();
        // z is shuffled second, so the other trigger's new x extent is checked.
        let is_in_x =
            self.is_in_x_extent(owner, self.x(other_lower), self.x(other_upper), abs_range);
        if !is_in_x {
            return;
        }
        let entity = self.trigger_entity(other_owner);
        let is_entering = self.nodes[node].is_lower_bound() == positive;
        if is_entering {
            let is_in_z =
                self.is_in_z_extent(owner, self.z(other_lower), self.z(other_upper), abs_range);
            if is_in_z {
                self.push_event(owner, entity, EventKind::Enter);
            }
        } else {
            let abs_old_range = self.node_old_range(node).abs();
            let was_in_x = self.was_in_x_extent(
                owner,
                self.bound_old_x(other_lower),
                self.bound_old_x(other_upper),
                abs_old_range,
            );
            let was_in_z = self.was_in_z_extent(
                owner,
                self.bound_old_z(other_lower),
                self.bound_old_z(other_upper),
                abs_old_range,
            );
            if was_in_x && was_in_z {
                self.push_event(owner, entity, EventKind::Leave);
            }
        }
    }

    /// `RangeTrigger::wasInXRange(entityLower, entityUpper, range)`
    /// (`range_trigger.hpp:154-166`): overlap of the old window with the
    /// other extent.
    fn was_in_x_extent(&self, owner: TriggerId, lower: f32, upper: f32, range: f32) -> bool {
        let sub = self.triggers[owner].old_entity_x;
        (sub - range) <= upper && lower <= (sub + range)
    }

    /// `RangeTrigger::wasInZRange(entityLower, entityUpper, range)`
    /// (`range_trigger.hpp:182-193`).
    fn was_in_z_extent(&self, owner: TriggerId, lower: f32, upper: f32, range: f32) -> bool {
        let sub = self.triggers[owner].old_entity_z;
        (sub - range) <= upper && lower <= (sub + range)
    }

    /// `RangeTrigger::isInXRange(entityLower, entityUpper, range)`
    /// (`range_trigger.hpp:168-180`): overlap with the current window.
    fn is_in_x_extent(&self, owner: TriggerId, lower: f32, upper: f32, range: f32) -> bool {
        let sub = self.x(self.triggers[owner].central);
        (sub - range) <= upper && lower <= (sub + range)
    }

    /// `RangeTrigger::isInZRange(entityLower, entityUpper, range)`
    /// (`range_trigger.hpp:195-207`).
    fn is_in_z_extent(&self, owner: TriggerId, lower: f32, upper: f32, range: f32) -> bool {
        let sub = self.z(self.triggers[owner].central);
        (sub - range) <= upper && lower <= (sub + range)
    }

    /// The entity a trigger belongs to: its central node's entity id
    /// (`RangeTrigger::pEntity` implementations).
    fn trigger_entity(&self, owner: TriggerId) -> EntityId {
        match self.nodes[self.triggers[owner].central].kind {
            NodeKind::Entity { id, .. } => id,
            _ => 0,
        }
    }

    fn push_event(&mut self, trigger: TriggerId, entity: EntityId, kind: EventKind) {
        self.events.push(RangeEvent {
            trigger,
            entity,
            kind,
        });
    }

    // ---- range predicates --------------------------------------------------

    /// `RangeTrigger::wasInXRange` (`range_trigger.hpp:98-108`): the
    /// half-open window centred on the OLD entity position,
    /// `(oldX - range, oldX + range]`.
    fn was_in_x_range(&self, owner: TriggerId, x: f32, range: f32) -> bool {
        let sub = self.triggers[owner].old_entity_x;
        (sub - range) < x && x <= (sub + range)
    }

    /// `RangeTrigger::wasInZRange` (`range_trigger.hpp:124-136`).
    fn was_in_z_range(&self, owner: TriggerId, z: f32, range: f32) -> bool {
        let sub = self.triggers[owner].old_entity_z;
        (sub - range) < z && z <= (sub + range)
    }

    /// `RangeTrigger::isInXRange` (`range_trigger.hpp:110-122`): the
    /// half-open window centred on the CURRENT entity position.
    fn is_in_x_range(&self, owner: TriggerId, x: f32, range: f32) -> bool {
        let sub = self.x(self.triggers[owner].central);
        (sub - range) < x && x <= (sub + range)
    }

    /// `RangeTrigger::isInZRange` (`range_trigger.hpp:138-150`).
    fn is_in_z_range(&self, owner: TriggerId, z: f32, range: f32) -> bool {
        let sub = self.z(self.triggers[owner].central);
        (sub - range) < z && z <= (sub + range)
    }

    /// `RangeTrigger::contains(float, float)` (`range_trigger.hpp:211-217`).
    pub fn trigger_contains(&self, owner: TriggerId, x: f32, z: f32) -> bool {
        let range = self.trigger_range(owner);
        self.is_in_x_range(owner, x, range) && self.is_in_z_range(owner, z, range)
    }

    /// `RangeTrigger::range()`: the upper bound's range
    /// (`range_trigger.hpp:209`).
    pub fn trigger_range(&self, owner: TriggerId) -> f32 {
        self.node_range(self.triggers[owner].upper)
    }

    /// The central entity's position.
    pub fn trigger_central_pos(&self, owner: TriggerId) -> Vector3 {
        match self.nodes[self.triggers[owner].central].kind {
            NodeKind::Entity { pos, .. } => pos,
            _ => Vector3::ZERO,
        }
    }

    // ---- shuffle -----------------------------------------------------------

    /// `RangeListNode::shuffleXThenZ` (`range_list_node.cpp:29-34`).
    fn shuffle_x_then_z(&mut self, node: NodeId, old_x: f32, old_z: f32) {
        self.shuffle_x(node, old_x, old_z);
        self.shuffle_z(node, old_x, old_z);
    }

    /// `RangeListNode::shuffleX` (`range_list_node.cpp:44-128`): walk the
    /// node leftward/rightward past misordered neighbours, firing crossings
    /// before each relink. The predicate is
    /// `our < other || (our == other && our.order <= other.order)`.
    fn shuffle_x(&mut self, node: NodeId, old_x: f32, old_z: f32) {
        let our = self.x(node);
        let our_order = self.nodes[node].order;

        // Shuffle to the left (negative X).
        while let Some(prev) = self.nodes[node].prev_x {
            let oth = self.x(prev);
            if !(our < oth || (is_equal(our, oth) && our_order <= self.nodes[prev].order)) {
                break;
            }
            let our_makes = self.nodes[node].makes;
            if self.nodes[node]
                .wants
                .wants_crossing_with(self.nodes[prev].makes)
            {
                self.crossed_x(node, prev, true, self.x(prev), self.z(prev));
            }
            if self.nodes[prev].wants.wants_crossing_with(our_makes) {
                self.crossed_x(prev, node, false, old_x, old_z);
            }
            // Unlink, then relink one slot to the left.
            let prev_prev = self.nodes[prev].prev_x;
            if let Some(n) = self.nodes[node].next_x {
                self.nodes[n].prev_x = Some(prev);
            }
            self.nodes[prev].next_x = self.nodes[node].next_x;
            self.nodes[node].next_x = Some(prev);
            self.nodes[node].prev_x = prev_prev;
            if let Some(p) = prev_prev {
                self.nodes[p].next_x = Some(node);
            }
            self.nodes[prev].prev_x = Some(node);
        }

        // Shuffle to the right (positive X).
        while let Some(next) = self.nodes[node].next_x {
            let oth = self.x(next);
            if !(our > oth || (is_equal(our, oth) && our_order >= self.nodes[next].order)) {
                break;
            }
            let our_makes = self.nodes[node].makes;
            if self.nodes[node]
                .wants
                .wants_crossing_with(self.nodes[next].makes)
            {
                self.crossed_x(node, next, false, self.x(next), self.z(next));
            }
            if self.nodes[next].wants.wants_crossing_with(our_makes) {
                self.crossed_x(next, node, true, old_x, old_z);
            }
            let next_next = self.nodes[next].next_x;
            if let Some(p) = self.nodes[node].prev_x {
                self.nodes[p].next_x = Some(next);
            }
            self.nodes[next].prev_x = self.nodes[node].prev_x;
            self.nodes[node].prev_x = Some(next);
            self.nodes[node].next_x = next_next;
            self.nodes[next].next_x = Some(node);
            if let Some(n) = next_next {
                self.nodes[n].prev_x = Some(node);
            }
        }
    }

    /// `RangeListNode::shuffleZ` (`range_list_node.cpp:135-219`).
    fn shuffle_z(&mut self, node: NodeId, old_x: f32, old_z: f32) {
        let our = self.z(node);
        let our_order = self.nodes[node].order;

        // Shuffle to the left (negative Z).
        while let Some(prev) = self.nodes[node].prev_z {
            let oth = self.z(prev);
            if !(our < oth || (is_equal(our, oth) && our_order <= self.nodes[prev].order)) {
                break;
            }
            let our_makes = self.nodes[node].makes;
            if self.nodes[node]
                .wants
                .wants_crossing_with(self.nodes[prev].makes)
            {
                self.crossed_z(node, prev, true, self.x(prev), self.z(prev));
            }
            if self.nodes[prev].wants.wants_crossing_with(our_makes) {
                self.crossed_z(prev, node, false, old_x, old_z);
            }
            let prev_prev = self.nodes[prev].prev_z;
            if let Some(n) = self.nodes[node].next_z {
                self.nodes[n].prev_z = Some(prev);
            }
            self.nodes[prev].next_z = self.nodes[node].next_z;
            self.nodes[node].next_z = Some(prev);
            self.nodes[node].prev_z = prev_prev;
            if let Some(p) = prev_prev {
                self.nodes[p].next_z = Some(node);
            }
            self.nodes[prev].prev_z = Some(node);
        }

        // Shuffle to the right (positive Z).
        while let Some(next) = self.nodes[node].next_z {
            let oth = self.z(next);
            if !(our > oth || (is_equal(our, oth) && our_order >= self.nodes[next].order)) {
                break;
            }
            let our_makes = self.nodes[node].makes;
            if self.nodes[node]
                .wants
                .wants_crossing_with(self.nodes[next].makes)
            {
                self.crossed_z(node, next, false, self.x(next), self.z(next));
            }
            if self.nodes[next].wants.wants_crossing_with(our_makes) {
                self.crossed_z(next, node, true, old_x, old_z);
            }
            let next_next = self.nodes[next].next_z;
            if let Some(p) = self.nodes[node].prev_z {
                self.nodes[p].next_z = Some(next);
            }
            self.nodes[next].prev_z = self.nodes[node].prev_z;
            self.nodes[node].prev_z = Some(next);
            self.nodes[node].next_z = next_next;
            self.nodes[next].next_z = Some(node);
            if let Some(n) = next_next {
                self.nodes[n].prev_z = Some(node);
            }
        }
    }

    // ---- public entity API -------------------------------------------------

    /// `RangeList::add` (`cell_range_list.cpp:36-45`): splice in just before
    /// the tail, then shuffle from `-FLT_MAX`.
    pub fn add_entity(&mut self, id: EntityId, pos: Vector3) -> NodeId {
        let node = self.nodes.len();
        self.nodes.push(Node {
            prev_x: None,
            next_x: None,
            prev_z: None,
            next_z: None,
            wants: Flags::NO_TRIGGERS,
            makes: Flags::ENTITY_TRIGGER
                .union(Flags::LOWER_AOI_TRIGGER)
                .union(Flags::UPPER_AOI_TRIGGER)
                .union(Flags::IS_ENTITY),
            order: order::ENTITY,
            kind: NodeKind::Entity {
                id,
                pos,
                removed: false,
            },
        });
        let head_next_x = self.nodes[self.head].next_x;
        self.insert_before_x(head_next_x.unwrap(), node);
        let head_next_z = self.nodes[self.head].next_z;
        self.insert_before_z(head_next_z.unwrap(), node);
        self.shuffle_x_then_z(node, -f32::MAX, -f32::MAX);
        node
    }

    /// Entity position (`EntityRangeListNode::x/z`,
    /// `entity_range_list_node.cpp:39-56`).
    pub fn entity_pos(&self, node: NodeId) -> Vector3 {
        self.entity_state(node).0
    }

    /// Entity id carried by a node.
    pub fn entity_id(&self, node: NodeId) -> EntityId {
        self.entity_state(node).1
    }

    /// Whether a node has been removed.
    pub fn is_removed(&self, node: NodeId) -> bool {
        self.entity_state(node).2
    }

    /// `(position, id, removed)` for an entity node; other kinds read as a
    /// pristine never-added entity.
    fn entity_state(&self, node: NodeId) -> (Vector3, EntityId, bool) {
        match self.nodes[node].kind {
            NodeKind::Entity { id, pos, removed } => (pos, id, removed),
            _ => (Vector3::ZERO, 0, false),
        }
    }

    /// Move an entity and follow all its triggers, exactly as
    /// `Entity::updateInternalsForNewPosition`
    /// (`entity.cpp:3963-4000`): leading bounds expand-shuffle forward in
    /// trigger order, the entity node shuffles, trailing bounds
    /// contract-shuffle in reverse trigger order. Old coordinates are the
    /// entity's previous position.
    pub fn move_entity(&mut self, node: NodeId, new_pos: Vector3) {
        let old = self.entity_pos(node);
        let increase_x = old.x < new_pos.x;
        let increase_z = old.z < new_pos.z;
        let owners: Vec<TriggerId> = (0..self.triggers.len())
            .filter(|t| self.triggers[*t].central == node)
            .collect();

        // The reference updates `globalPosition_` before
        // `updateInternalsForNewPosition` runs, so the central node already
        // reports the new position while the bounds shuffle.
        self.set_entity_pos(node, new_pos);
        for owner in owners.iter() {
            self.trigger_shuffle_x_then_z_expand(*owner, increase_x, increase_z, old.x, old.z);
        }
        self.shuffle_x_then_z(node, old.x, old.z);
        for owner in owners.iter().rev() {
            self.trigger_shuffle_x_then_z_contract(*owner, increase_x, increase_z, old.x, old.z);
        }
    }

    fn set_entity_pos(&mut self, node: NodeId, pos: Vector3) {
        if let NodeKind::Entity { pos: slot, .. } = &mut self.nodes[node].kind {
            *slot = pos;
        }
    }

    /// `EntityRangeListNode::remove` (`entity_range_list_node.cpp:74-86`):
    /// teleport to z = FLT_MAX, shuffle to fire the leave crossings, then
    /// unlink.
    pub fn remove_entity(&mut self, node: NodeId) {
        let pos = self.entity_pos(node);
        let old_z = pos.z;
        let shifted = Vector3 {
            x: pos.x,
            y: pos.y,
            z: f32::MAX,
        };
        self.set_entity_pos(node, shifted);
        self.shuffle_z(node, pos.x, old_z);
        self.unlink(node);
        if let NodeKind::Entity { removed, .. } = &mut self.nodes[node].kind {
            *removed = true;
        }
    }

    // ---- trigger API -------------------------------------------------------

    /// Generic `RangeTrigger` construction (`range_trigger.cpp:433-447`).
    /// The bounds splice in only at [`RangeList::trigger_insert`], mirroring
    /// the reference's two-phase construction.
    #[allow(clippy::too_many_arguments)]
    pub fn add_trigger(
        &mut self,
        central: NodeId,
        range: f32,
        wants_lower: Flags,
        wants_upper: Flags,
        makes_lower: Flags,
        makes_upper: Flags,
    ) -> TriggerId {
        let owner = self.triggers.len();
        let lower = self.nodes.len();
        self.nodes.push(Node {
            prev_x: None,
            next_x: None,
            prev_z: None,
            next_z: None,
            wants: wants_lower,
            makes: makes_lower.union(Flags::IS_LOWER_BOUND),
            order: order::LOWER_BOUND,
            kind: NodeKind::Trigger {
                owner,
                range: -range,
                old_range: 0.0,
            },
        });
        let upper = self.nodes.len();
        self.nodes.push(Node {
            prev_x: None,
            next_x: None,
            prev_z: None,
            next_z: None,
            wants: wants_upper,
            makes: makes_upper,
            order: order::UPPER_BOUND,
            kind: NodeKind::Trigger {
                owner,
                range,
                old_range: 0.0,
            },
        });
        let (old_x, old_z) = match self.nodes[central].kind {
            NodeKind::Entity { pos, .. } => (pos.x, pos.z),
            _ => (0.0, 0.0),
        };
        self.triggers.push(TriggerData {
            central,
            lower,
            upper,
            old_entity_x: old_x,
            old_entity_z: old_z,
        });
        owner
    }

    /// `AoITrigger` (`witness.cpp:73-101`): wants the AOI flags from entity
    /// nodes, makes none.
    pub fn add_aoi_trigger(&mut self, central: NodeId, range: f32) -> TriggerId {
        self.add_trigger(
            central,
            range,
            Flags::LOWER_AOI_TRIGGER,
            Flags::UPPER_AOI_TRIGGER,
            Flags::NO_TRIGGERS,
            Flags::NO_TRIGGERS,
        )
    }

    /// `RangeListAppealTrigger`
    /// (`range_list_appeal_trigger.cpp:23-33`): wants nothing, makes the
    /// AOI flags so witnesses' AOI triggers detect large entities beyond
    /// their own radius. Note the reference swaps them: the LOWER bound
    /// makes UPPER_AOI crossings and vice versa, so a witness's leading AOI
    /// bound pairs with the appeal's trailing bound.
    pub fn add_appeal_trigger(&mut self, central: NodeId, range: f32) -> TriggerId {
        self.add_trigger(
            central,
            range,
            Flags::NO_TRIGGERS,
            Flags::NO_TRIGGERS,
            Flags::UPPER_AOI_TRIGGER,
            Flags::LOWER_AOI_TRIGGER,
        )
    }

    /// `RangeTrigger::insert` (`range_trigger.cpp:464-534`): splice the
    /// bounds just past the co-located run of nodes, then shuffle both bounds
    /// out from zero range. This is what produces the initial enter events
    /// for entities already inside the radius.
    pub fn trigger_insert(&mut self, owner: TriggerId) {
        let central = self.triggers[owner].central;
        let upper_order = self.nodes[self.triggers[owner].upper].order;

        // Walk the X cursor past nodes co-located with the central node whose
        // order sorts before the upper bound.
        let mut cursor = self.nodes[central].next_x;
        while let Some(c) = cursor {
            if !(is_equal(self.x(c), self.x(central)) && self.nodes[c].order < upper_order) {
                break;
            }
            cursor = self.nodes[c].next_x;
        }
        let anchor_x = cursor.unwrap();
        self.insert_before_x(anchor_x, self.triggers[owner].lower);
        self.insert_before_x(anchor_x, self.triggers[owner].upper);

        let mut cursor = self.nodes[central].next_z;
        while let Some(c) = cursor {
            if !(is_equal(self.z(c), self.z(central)) && self.nodes[c].order < upper_order) {
                break;
            }
            cursor = self.nodes[c].next_z;
        }
        let anchor_z = cursor.unwrap();
        self.insert_before_z(anchor_z, self.triggers[owner].lower);
        self.insert_before_z(anchor_z, self.triggers[owner].upper);

        let old_x = self.x(central);
        let old_z = self.z(central);
        self.triggers[owner].old_entity_x = old_x;
        self.triggers[owner].old_entity_z = old_z;

        // Initial shuffle from zero range.
        let (lower, upper) = (self.triggers[owner].lower, self.triggers[owner].upper);
        self.bound_set_old_range(upper, 0.0);
        self.bound_set_old_range(lower, 0.0);
        self.shuffle_x(upper, old_x, old_z);
        self.shuffle_x(lower, old_x, old_z);
        self.shuffle_z(upper, old_x, old_z);
        self.shuffle_z(lower, old_x, old_z);
        self.bound_update_old_range(upper);
        self.bound_update_old_range(lower);
    }

    fn bound_set_old_range(&mut self, node: NodeId, value: f32) {
        if let NodeKind::Trigger { old_range, .. } = &mut self.nodes[node].kind {
            *old_range = value;
        }
    }

    fn bound_update_old_range(&mut self, node: NodeId) {
        let range = self.node_range(node);
        self.bound_set_old_range(node, range);
    }

    /// `RangeTriggerNode::setRange` (`range_trigger.cpp:408-416`): record the
    /// old extent, change the range and reshuffle so shrinking fires leaves
    /// and growing fires enters.
    fn bound_set_range(&mut self, node: NodeId, range: f32) {
        let old_range = self.node_range(node);
        self.bound_set_old_range(node, old_range);
        let old_x = self.x(node);
        let old_z = self.z(node);
        if let NodeKind::Trigger { range: slot, .. } = &mut self.nodes[node].kind {
            *slot = range;
        }
        self.shuffle_x_then_z(node, old_x, old_z);
        self.bound_set_old_range(node, range);
    }

    /// `RangeTrigger::setRange` (`range_trigger.cpp:632-639`): clamp to a
    /// minimum of 0.001 and apply to both bounds.
    pub fn trigger_set_range(&mut self, owner: TriggerId, range: f32) {
        let r = if range < 0.001 { 0.001 } else { range };
        let lower = self.triggers[owner].lower;
        let upper = self.triggers[owner].upper;
        self.bound_set_range(upper, r);
        self.bound_set_range(lower, -r);
    }

    /// `RangeTrigger::remove` (`range_trigger.cpp:541-550`): contract to a
    /// point (firing leaves), then unlink.
    pub fn trigger_remove(&mut self, owner: TriggerId) {
        let our = self.node_range(self.triggers[owner].upper);
        let olr = self.node_range(self.triggers[owner].lower);
        let lower = self.triggers[owner].lower;
        let upper = self.triggers[owner].upper;
        self.bound_set_range(upper, 0.0);
        self.bound_set_range(lower, 0.0);
        self.trigger_remove_without_contracting(owner);
        if let NodeKind::Trigger { range, .. } = &mut self.nodes[upper].kind {
            *range = our;
        }
        if let NodeKind::Trigger { range, .. } = &mut self.nodes[lower].kind {
            *range = olr;
        }
    }

    /// `RangeTrigger::removeWithoutContracting`
    /// (`range_trigger.cpp:557-561`): unlink the bounds without firing
    /// leaves.
    pub fn trigger_remove_without_contracting(&mut self, owner: TriggerId) {
        let lower = self.triggers[owner].lower;
        let upper = self.triggers[owner].upper;
        self.unlink(lower);
        self.unlink(upper);
    }

    /// `RangeTrigger::shuffleXThenZ` (`range_trigger.cpp:570-581`): both
    /// bounds follow their central entity, keeping `oldEntityX_/Z_` in sync.
    pub fn trigger_shuffle_x_then_z(&mut self, owner: TriggerId, old_x: f32, old_z: f32) {
        let lower = self.triggers[owner].lower;
        let upper = self.triggers[owner].upper;
        let upper_range = self.node_range(upper);
        let lower_range = self.node_range(lower);
        self.shuffle_x_then_z(upper, old_x + upper_range, old_z + upper_range);
        self.shuffle_x_then_z(lower, old_x + lower_range, old_z + lower_range);
        let central = self.triggers[owner].central;
        let (cx, cz) = (self.x(central), self.z(central));
        self.triggers[owner].old_entity_x = cx;
        self.triggers[owner].old_entity_z = cz;
    }

    /// `RangeTrigger::shuffleXThenZExpand` (`range_trigger.cpp:588-601`):
    /// shuffle only the bounds that move outward for this direction of
    /// travel.
    pub fn trigger_shuffle_x_then_z_expand(
        &mut self,
        owner: TriggerId,
        x_inc: bool,
        z_inc: bool,
        old_x: f32,
        old_z: f32,
    ) {
        let lower = self.triggers[owner].lower;
        let upper = self.triggers[owner].upper;
        let x_trigger = if x_inc { upper } else { lower };
        let x_range = self.node_range(x_trigger);
        self.shuffle_x(x_trigger, old_x + x_range, old_z + x_range);
        let z_trigger = if z_inc { upper } else { lower };
        let z_range = self.node_range(z_trigger);
        self.shuffle_z(z_trigger, old_x + z_range, old_z + z_range);
    }

    /// `RangeTrigger::shuffleXThenZContract` (`range_trigger.cpp:608-624`):
    /// shuffle only the trailing bounds, then refresh the old position.
    pub fn trigger_shuffle_x_then_z_contract(
        &mut self,
        owner: TriggerId,
        x_inc: bool,
        z_inc: bool,
        old_x: f32,
        old_z: f32,
    ) {
        let lower = self.triggers[owner].lower;
        let upper = self.triggers[owner].upper;
        let x_trigger = if x_inc { lower } else { upper };
        let x_range = self.node_range(x_trigger);
        self.shuffle_x(x_trigger, old_x + x_range, old_z + x_range);
        let z_trigger = if z_inc { lower } else { upper };
        let z_range = self.node_range(z_trigger);
        self.shuffle_z(z_trigger, old_x + z_range, old_z + z_range);
        let central = self.triggers[owner].central;
        let (cx, cz) = (self.x(central), self.z(central));
        self.triggers[owner].old_entity_x = cx;
        self.triggers[owner].old_entity_z = cz;
    }

    /// `visitLargeEntities` support for `AoITrigger` construction
    /// (`witness.cpp:93-95`): the entities whose appeal range covers this
    /// point join the AoI immediately at witness creation.
    pub fn appeal_entities_containing(&self, x: f32, z: f32) -> Vec<EntityId> {
        let mut out = Vec::new();
        for data in &self.triggers {
            // Appeal triggers are the ones whose bounds make AOI crossings
            // (`range_list_appeal_trigger.cpp:26-31`); witness AOI bounds
            // make nothing.
            if !self.nodes[data.lower]
                .makes
                .wants_crossing_with(Flags::UPPER_AOI_TRIGGER)
            {
                continue;
            }
            let range = self.trigger_range_of(data);
            if range <= 0.0
                || !self.is_in_x_range_of(data, x, range)
                || !self.is_in_z_range_of(data, z, range)
            {
                continue;
            }
            if let NodeKind::Entity { id, .. } = self.nodes[data.central].kind {
                out.push(id);
            }
        }
        out.sort_unstable();
        out.dedup();
        out
    }

    /// Range of a trigger from its data, usable mid-iteration.
    fn trigger_range_of(&self, data: &TriggerData) -> f32 {
        self.node_range(data.upper)
    }

    fn is_in_x_range_of(&self, data: &TriggerData, x: f32, range: f32) -> bool {
        let sub = self.x(data.central);
        (sub - range) < x && x <= (sub + range)
    }

    fn is_in_z_range_of(&self, data: &TriggerData, z: f32, range: f32) -> bool {
        let sub = self.z(data.central);
        (sub - range) < z && z <= (sub + range)
    }

    // ---- events and invariants ----------------------------------------------

    /// Drain the enter/leave event log.
    pub fn drain_events(&mut self) -> Vec<RangeEvent> {
        std::mem::take(&mut self.events)
    }

    /// Drain the events belonging to one trigger, leaving every other
    /// trigger's events queued. The reference dispatches each crossing
    /// straight to its own trigger's handler, so one witness never consumes
    /// another's enter/leave callbacks.
    pub fn drain_events_for(&mut self, trigger: TriggerId) -> Vec<RangeEvent> {
        let mut mine = Vec::new();
        let mut rest = Vec::new();
        for event in std::mem::take(&mut self.events) {
            if event.trigger == trigger {
                mine.push(event);
            } else {
                rest.push(event);
            }
        }
        self.events = rest;
        mine
    }

    /// `RangeList::isSorted` (`cell_range_list.cpp:51-118`): both axis lists
    /// ascend by coordinate with the order tie-break.
    pub fn is_sorted(&self) -> bool {
        let mut prev = self.head;
        let mut iter = self.nodes[self.head].next_x;
        while let Some(i) = iter {
            let ascends = self.x(prev) < self.x(i)
                || (self.x(prev) == self.x(i) && self.nodes[prev].order <= self.nodes[i].order);
            if !ascends {
                return false;
            }
            prev = i;
            iter = self.nodes[i].next_x;
        }
        prev = self.head;
        iter = self.nodes[self.head].next_z;
        while let Some(i) = iter {
            let ascends = self.z(prev) < self.z(i)
                || (self.z(prev) == self.z(i) && self.nodes[prev].order <= self.nodes[i].order);
            if !ascends {
                return false;
            }
            prev = i;
            iter = self.nodes[i].next_z;
        }
        true
    }

    /// Walk the X list from the head to the tail, yielding node ids
    /// (debug/tests). The terminators themselves are not included.
    pub fn iter_x(&self) -> Vec<NodeId> {
        let mut out = Vec::new();
        let mut cursor = self.nodes[self.head].next_x;
        while let Some(c) = cursor {
            if c == self.tail {
                break;
            }
            out.push(c);
            cursor = self.nodes[c].next_x;
        }
        out
    }

    /// Walk the Z list from the head to the tail (debug/tests).
    pub fn iter_z(&self) -> Vec<NodeId> {
        let mut out = Vec::new();
        let mut cursor = self.nodes[self.head].next_z;
        while let Some(c) = cursor {
            if c == self.tail {
                break;
            }
            out.push(c);
            cursor = self.nodes[c].next_z;
        }
        out
    }

    /// Number of live entity nodes.
    pub fn entity_count(&self) -> usize {
        self.nodes
            .iter()
            .filter(|n| matches!(n.kind, NodeKind::Entity { removed: false, .. }))
            .count()
    }

    /// Number of triggers.
    pub fn trigger_count(&self) -> usize {
        self.triggers.len()
    }
}

impl Default for RangeList {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(x: f32, z: f32) -> Vector3 {
        Vector3 { x, y: 0.0, z }
    }

    /// A witness entity with an AOI trigger inserted at `pos`; returns the
    /// trigger id.
    fn witness_with_radius(list: &mut RangeList, pos: Vector3, range: f32) -> TriggerId {
        let witness = list.add_entity(1, pos);
        let aoi = list.add_aoi_trigger(witness, range);
        list.trigger_insert(aoi);
        aoi
    }

    fn kinds(events: &[RangeEvent]) -> Vec<EventKind> {
        events.iter().map(|e| e.kind).collect()
    }

    fn entities(events: &[RangeEvent]) -> Vec<EntityId> {
        events.iter().map(|e| e.entity).collect()
    }

    #[test]
    fn empty_list_invariants() {
        let mut list = RangeList::new();
        assert!(list.is_sorted());
        assert_eq!(list.entity_count(), 0);
        assert_eq!(list.trigger_count(), 0);
        assert!(list.iter_x().is_empty());
        assert!(list.iter_z().is_empty());
        assert!(list.drain_events().is_empty());
    }

    #[test]
    fn flags_want_make_pairs() {
        // An entity makes entity and AOI crossings but wants none.
        let entity_makes = Flags::ENTITY_TRIGGER
            .union(Flags::LOWER_AOI_TRIGGER)
            .union(Flags::UPPER_AOI_TRIGGER)
            .union(Flags::IS_ENTITY);
        assert!(!Flags::NO_TRIGGERS.wants_crossing_with(entity_makes));
        assert!(!entity_makes.wants_crossing_with(Flags::NO_TRIGGERS));
        // An AOI lower bound wants LOWER_AOI crossings from entities.
        assert!(Flags::LOWER_AOI_TRIGGER.wants_crossing_with(entity_makes));
        assert!(Flags::UPPER_AOI_TRIGGER.wants_crossing_with(entity_makes));
        assert!(!Flags::UPPER_AOI_TRIGGER.wants_crossing_with(Flags::NO_TRIGGERS));
        assert!(Flags::IS_ENTITY.contains(Flags::IS_ENTITY));
        assert!(!Flags::NO_TRIGGERS.contains(Flags::IS_ENTITY));
        assert_eq!(Flags::LOWER_AOI_TRIGGER.union(Flags::IS_ENTITY).0, 0x12);
        assert_eq!(order::HEAD, 0);
        assert_eq!(order::ENTITY, 100);
        assert_eq!(order::LOWER_BOUND, 190);
        assert_eq!(order::UPPER_BOUND, 200);
        assert_eq!(order::TAIL, 0xffff);
    }

    /// BW-08-03-01 acceptance: the critical radius boundary fires enter for
    /// entities inside and exactly at the upper bound, and nothing beyond.
    #[test]
    fn insert_enter_matches_radius_boundary() {
        let mut list = RangeList::new();
        witness_with_radius(&mut list, v(0.0, 0.0), 10.0);
        list.drain_events();

        // Exactly at the upper bound: inside (the window is half-open
        // `(center - r, center + r]`).
        list.add_entity(2, v(10.0, 0.0));
        // Just beyond the upper bound: outside.
        list.add_entity(3, v(10.5, 0.0));
        // Inside on the lower half.
        list.add_entity(4, v(-9.5, 0.0));
        // Exactly at the lower bound: outside (strict lower bound).
        list.add_entity(5, v(-10.0, 0.0));

        let events = list.drain_events();
        assert_eq!(entities(&events), vec![2, 4], "critical-radius enters");
        assert!(kinds(&events).iter().all(|k| *k == EventKind::Enter));
        assert!(list.is_sorted());
    }

    /// BW-08-03-01 acceptance: walking an entity across the radius fires
    /// leave exactly when it exits and enter exactly when it re-enters.
    #[test]
    fn walk_through_radius_enter_leave_sequence() {
        let mut list = RangeList::new();
        let f = witness_with_radius(&mut list, v(0.0, 0.0), 10.0);
        list.drain_events();
        let b = list.add_entity(2, v(0.0, 0.0));
        list.drain_events();

        // Walk right out through the upper bound.
        list.move_entity(b, v(20.0, 0.0));
        assert_eq!(
            list.drain_events(),
            vec![RangeEvent {
                trigger: f,
                entity: 2,
                kind: EventKind::Leave,
            }]
        );

        // Walk back in: enter fires crossing the upper bound.
        list.move_entity(b, v(0.0, 0.0));
        assert_eq!(
            list.drain_events(),
            vec![RangeEvent {
                trigger: f,
                entity: 2,
                kind: EventKind::Enter,
            }]
        );

        // Walk out through the lower bound.
        list.move_entity(b, v(-20.0, 0.0));
        assert_eq!(
            list.drain_events(),
            vec![RangeEvent {
                trigger: f,
                entity: 2,
                kind: EventKind::Leave,
            }]
        );

        // And back in again.
        list.move_entity(b, v(0.0, 0.0));
        assert_eq!(
            list.drain_events(),
            vec![RangeEvent {
                trigger: f,
                entity: 2,
                kind: EventKind::Enter,
            }]
        );
        assert!(list.is_sorted());
    }

    /// The upper radius bound is inclusive under movement: a step from
    /// exactly `r` to just past it fires the leave, and returning to exactly
    /// `r` fires the enter.
    #[test]
    fn upper_boundary_step_is_inclusive() {
        let mut list = RangeList::new();
        witness_with_radius(&mut list, v(0.0, 0.0), 10.0);
        list.drain_events();
        let b = list.add_entity(2, v(20.0, 0.0));
        assert!(list.drain_events().is_empty());

        // In to exactly the boundary: enter.
        list.move_entity(b, v(10.0, 0.0));
        assert_eq!(kinds(&list.drain_events()), vec![EventKind::Enter]);

        // Still exactly on the boundary: no further event.
        list.move_entity(b, v(10.0, 0.0));
        assert!(list.drain_events().is_empty());

        // One epsilon past it: leave.
        list.move_entity(b, v(10.000001, 0.0));
        assert_eq!(kinds(&list.drain_events()), vec![EventKind::Leave]);

        // Back onto the boundary: enter again.
        list.move_entity(b, v(10.0, 0.0));
        assert_eq!(kinds(&list.drain_events()), vec![EventKind::Enter]);
        assert!(list.is_sorted());
    }

    /// The same critical boundary behaviour along the Z axis.
    #[test]
    fn z_axis_boundary_sequence() {
        let mut list = RangeList::new();
        witness_with_radius(&mut list, v(0.0, 0.0), 10.0);
        list.drain_events();
        let b = list.add_entity(2, v(0.0, 20.0));
        assert!(list.drain_events().is_empty());

        list.move_entity(b, v(0.0, 10.0));
        assert_eq!(kinds(&list.drain_events()), vec![EventKind::Enter]);

        list.move_entity(b, v(0.0, -20.0));
        assert_eq!(kinds(&list.drain_events()), vec![EventKind::Leave]);

        list.move_entity(b, v(0.0, 0.0));
        assert_eq!(kinds(&list.drain_events()), vec![EventKind::Enter]);
        assert!(list.is_sorted());
    }

    /// A diagonal walk fires exactly one enter: the X shuffle checks the old
    /// Z (outside) and the Z shuffle then confirms with the new X.
    #[test]
    fn diagonal_walk_single_enter() {
        let mut list = RangeList::new();
        let f = witness_with_radius(&mut list, v(0.0, 0.0), 10.0);
        list.drain_events();
        let b = list.add_entity(2, v(20.0, 20.0));
        assert!(list.drain_events().is_empty());

        list.move_entity(b, v(0.0, 0.0));
        let events = list.drain_events();
        assert_eq!(events.len(), 1, "one event, not one per axis");
        assert_eq!(events[0].trigger, f);
        assert_eq!(events[0].entity, 2);
        assert_eq!(events[0].kind, EventKind::Enter);
        assert!(list.is_sorted());
    }

    /// Co-located entities and a tiny radius: `RangeTrigger::insert`'s
    /// cursor walk must not produce spurious leaves
    /// (`range_trigger.cpp:466-488`).
    #[test]
    fn colocated_tiny_radius_no_spurious_events() {
        let mut list = RangeList::new();
        let a = list.add_entity(1, v(0.0, 0.0));
        list.add_entity(2, v(0.0, 0.0));
        // Entities just inside/outside a 0.001 radius.
        list.add_entity(3, v(0.0005, 0.0));
        list.add_entity(4, v(-0.0005, 0.0));
        list.add_entity(5, v(0.002, 0.0));
        list.add_entity(6, v(-0.001, 0.0));

        let aoi = list.add_aoi_trigger(a, 0.001);
        list.trigger_insert(aoi);

        let events = list.drain_events();
        // Entity 2 stands exactly at the witness position and joins (the
        // window is (centre - r, centre + r] on both axes); 3 and 4 are
        // inside; 6 sits exactly at the strict lower bound and 5 beyond the
        // upper, so neither joins.
        let mut entered = entities(&events);
        entered.sort_unstable();
        assert_eq!(entered, vec![2, 3, 4]);
        assert!(kinds(&events).iter().all(|k| *k == EventKind::Enter));
        assert!(list.is_sorted());
    }

    /// Shrinking the radius past a contained entity fires a leave; growing it
    /// back fires an enter (`RangeTrigger::setRange`).
    #[test]
    fn set_range_shrink_and_grow() {
        let mut list = RangeList::new();
        let f = witness_with_radius(&mut list, v(0.0, 0.0), 10.0);
        list.drain_events();
        list.add_entity(2, v(5.0, 0.0));
        assert_eq!(kinds(&list.drain_events()), vec![EventKind::Enter]);

        list.trigger_set_range(f, 4.0);
        assert_eq!(
            list.drain_events(),
            vec![RangeEvent {
                trigger: f,
                entity: 2,
                kind: EventKind::Leave,
            }]
        );
        assert_eq!(list.trigger_range(f), 4.0);

        list.trigger_set_range(f, 6.0);
        assert_eq!(
            list.drain_events(),
            vec![RangeEvent {
                trigger: f,
                entity: 2,
                kind: EventKind::Enter,
            }]
        );
        assert!(list.is_sorted());
    }

    /// `RangeTrigger::setRange` clamps to a minimum of 0.001.
    #[test]
    fn set_range_clamps_to_minimum() {
        let mut list = RangeList::new();
        let a = list.add_entity(1, v(0.0, 0.0));
        let aoi = list.add_aoi_trigger(a, 5.0);
        list.trigger_insert(aoi);
        list.drain_events();

        list.trigger_set_range(aoi, 0.0);
        assert_eq!(list.trigger_range(aoi), 0.001);
        assert!(list.is_sorted());
    }

    /// `EntityRangeListNode::remove`: teleporting to z = FLT_MAX fires the
    /// leave crossings, then unlinks.
    #[test]
    fn remove_entity_fires_leave_and_unlinks() {
        let mut list = RangeList::new();
        let f = witness_with_radius(&mut list, v(0.0, 0.0), 10.0);
        list.drain_events();
        let b = list.add_entity(2, v(5.0, 0.0));
        assert_eq!(kinds(&list.drain_events()), vec![EventKind::Enter]);
        assert_eq!(list.entity_count(), 2);

        list.remove_entity(b);
        assert_eq!(
            list.drain_events(),
            vec![RangeEvent {
                trigger: f,
                entity: 2,
                kind: EventKind::Leave,
            }]
        );
        assert_eq!(list.entity_count(), 1);
        assert!(list.is_removed(b));
        assert_eq!(list.entity_id(b), 2);
        assert!(list.is_sorted());
        // The witness node plus its two AOI bounds remain linked.
        assert_eq!(list.iter_x().len(), 3);
        assert_eq!(list.iter_z().len(), 3);
    }

    /// `RangeTrigger::remove` contracts to zero first, firing leaves for
    /// everything still inside.
    #[test]
    fn trigger_remove_fires_leaves() {
        let mut list = RangeList::new();
        let f = witness_with_radius(&mut list, v(0.0, 0.0), 10.0);
        list.drain_events();
        list.add_entity(2, v(5.0, 0.0));
        list.add_entity(3, v(-5.0, 0.0));
        assert_eq!(
            kinds(&list.drain_events()),
            vec![EventKind::Enter, EventKind::Enter]
        );

        list.trigger_remove(f);
        let events = list.drain_events();
        assert_eq!(entities(&events), vec![2, 3]);
        assert!(kinds(&events).iter().all(|k| *k == EventKind::Leave));
        // The range is restored after the contraction.
        assert_eq!(list.trigger_range(f), 10.0);
        assert!(list.is_sorted());
        // The witness entity node itself is still linked.
        assert_eq!(list.entity_count(), 3);
        assert_eq!(list.iter_x().len(), 3);
    }

    /// `removeWithoutContracting` unlinks silently (entity destruction with
    /// the trigger going away with it).
    #[test]
    fn trigger_remove_without_contracting_is_silent() {
        let mut list = RangeList::new();
        let f = witness_with_radius(&mut list, v(0.0, 0.0), 10.0);
        list.drain_events();
        list.add_entity(2, v(5.0, 0.0));
        list.drain_events();

        list.trigger_remove_without_contracting(f);
        assert!(list.drain_events().is_empty());
        assert!(list.is_sorted());
        assert_eq!(list.iter_x().len(), 2);
    }

    /// Large entities: a witness whose AOI walks into an appeal range sees
    /// the large entity, and stops seeing it once past the far edge
    /// (`crossedXEntityRange`, the overlap forms).
    #[test]
    fn appeal_range_overlap_enter_and_leave() {
        let mut list = RangeList::new();
        let large = list.add_entity(9, v(100.0, 0.0));
        let appeal = list.add_appeal_trigger(large, 50.0);
        list.trigger_insert(appeal);
        list.drain_events();

        let w = list.add_entity(1, v(40.0, 0.0));
        let aoi = list.add_aoi_trigger(w, 5.0);
        list.trigger_insert(aoi);
        // Far outside the appeal range: nothing yet.
        assert!(list.drain_events().is_empty());

        // Walk right into the appeal range [50, 150].
        list.move_entity(w, v(60.0, 0.0));
        assert_eq!(
            list.drain_events(),
            vec![RangeEvent {
                trigger: aoi,
                entity: 9,
                kind: EventKind::Enter,
            }]
        );

        // Walk past the far edge.
        list.move_entity(w, v(160.0, 0.0));
        assert_eq!(
            list.drain_events(),
            vec![RangeEvent {
                trigger: aoi,
                entity: 9,
                kind: EventKind::Leave,
            }]
        );
        assert!(list.is_sorted());
    }

    /// A teleport far past an appeal range fires the crossing walk but the
    /// final-extent checks reject both enter and leave: the witness was
    /// never inside and does not end inside.
    #[test]
    fn teleport_past_appeal_x_axis_no_events() {
        let mut list = RangeList::new();
        let large = list.add_entity(9, v(100.0, 0.0));
        let appeal = list.add_appeal_trigger(large, 50.0);
        list.trigger_insert(appeal);
        let w = list.add_entity(1, v(0.0, 0.0));
        let aoi = list.add_aoi_trigger(w, 5.0);
        list.trigger_insert(aoi);
        assert!(list.drain_events().is_empty());

        list.move_entity(w, v(200.0, 0.0));
        assert!(list.drain_events().is_empty());
        assert!(list.is_sorted());
    }

    /// The teleport-past check along the Z axis.
    #[test]
    fn teleport_past_appeal_z_axis_no_events() {
        let mut list = RangeList::new();
        let large = list.add_entity(9, v(0.0, 100.0));
        let appeal = list.add_appeal_trigger(large, 50.0);
        list.trigger_insert(appeal);
        let w = list.add_entity(1, v(0.0, 0.0));
        let aoi = list.add_aoi_trigger(w, 5.0);
        list.trigger_insert(aoi);
        assert!(list.drain_events().is_empty());

        list.move_entity(w, v(0.0, 200.0));
        assert!(list.drain_events().is_empty());
        assert!(list.is_sorted());
    }

    /// The same large-entity detection along the Z axis
    /// (`crossedZEntityRange`).
    #[test]
    fn appeal_range_overlap_along_z_axis() {
        let mut list = RangeList::new();
        let large = list.add_entity(9, v(0.0, 100.0));
        let appeal = list.add_appeal_trigger(large, 50.0);
        list.trigger_insert(appeal);
        list.drain_events();

        let w = list.add_entity(1, v(0.0, 40.0));
        let aoi = list.add_aoi_trigger(w, 5.0);
        list.trigger_insert(aoi);
        assert!(list.drain_events().is_empty());

        // Walk up into the appeal range.
        list.move_entity(w, v(0.0, 60.0));
        assert_eq!(
            list.drain_events(),
            vec![RangeEvent {
                trigger: aoi,
                entity: 9,
                kind: EventKind::Enter,
            }]
        );

        // Walk past the far edge.
        list.move_entity(w, v(0.0, 160.0));
        assert_eq!(
            list.drain_events(),
            vec![RangeEvent {
                trigger: aoi,
                entity: 9,
                kind: EventKind::Leave,
            }]
        );
        assert!(list.is_sorted());
    }

    /// `visitLargeEntities`: appeal entities covering a point are reported
    /// so a freshly created witness can seed its AoI.
    #[test]
    fn appeal_entities_containing_point() {
        let mut list = RangeList::new();
        let large = list.add_entity(9, v(100.0, 0.0));
        let appeal = list.add_appeal_trigger(large, 50.0);
        list.trigger_insert(appeal);
        let w = list.add_entity(1, v(0.0, 0.0));
        let aoi = list.add_aoi_trigger(w, 5.0);
        list.trigger_insert(aoi);
        list.drain_events();

        assert_eq!(list.appeal_entities_containing(70.0, 0.0), vec![9]);
        assert_eq!(list.appeal_entities_containing(150.0, 0.0), vec![9]);
        assert_eq!(list.appeal_entities_containing(50.0, 0.0), vec![]);
        assert_eq!(list.appeal_entities_containing(200.0, 0.0), vec![]);
        assert_eq!(list.appeal_entities_containing(70.0, 100.0), vec![]);
        // The witness AOI trigger is not an appeal trigger.
        assert_eq!(list.trigger_count(), 2);
        assert_eq!(list.trigger_range(appeal), 50.0);
        assert_eq!(list.trigger_range(aoi), 5.0);
        assert_eq!(list.trigger_central_pos(appeal), v(100.0, 0.0));
    }

    /// The plain `RangeTrigger::shuffleXThenZ` face: bounds track a moved
    /// central entity without spurious events when nothing is nearby.
    #[test]
    fn trigger_shuffle_tracks_central() {
        let mut list = RangeList::new();
        let a = list.add_entity(1, v(0.0, 0.0));
        let aoi = list.add_aoi_trigger(a, 10.0);
        list.trigger_insert(aoi);
        list.add_entity(2, v(100.0, 0.0));
        list.drain_events();

        // Move the central entity without the entity-node shuffle, then
        // follow with the trigger face.
        list.move_entity(a, v(5.0, 0.0));
        assert!(list.drain_events().is_empty());
        assert_eq!(list.trigger_central_pos(aoi), v(5.0, 0.0));
        assert!(list.trigger_contains(aoi, 12.0, 0.0));
        assert!(!list.trigger_contains(aoi, 16.0, 0.0));
        assert_eq!(list.entity_pos(a), v(5.0, 0.0));
        assert!(list.is_sorted());

        // The plain face with a no-op follow: bounds stay put, no events.
        list.trigger_shuffle_x_then_z(aoi, 5.0, 0.0);
        assert!(list.drain_events().is_empty());
        assert!(list.is_sorted());
        assert_eq!(list.trigger_central_pos(aoi), v(5.0, 0.0));
    }

    /// A mixed churn of adds, moves and removals keeps both axis lists
    /// sorted and the node counts consistent.
    #[test]
    fn sorted_invariant_across_churn() {
        let mut list = RangeList::new();
        witness_with_radius(&mut list, v(0.0, 0.0), 8.0);
        list.drain_events();

        let mut nodes = Vec::new();
        for i in 0..6u32 {
            let n = list.add_entity(10 + i, v(-30.0 + 10.0 * i as f32, -20.0 + 7.0 * i as f32));
            nodes.push(n);
            assert!(list.is_sorted());
        }
        assert_eq!(list.entity_count(), 7);

        for (step, n) in nodes.iter().enumerate() {
            list.move_entity(*n, v(20.0 - 5.0 * step as f32, 10.0 + 3.0 * step as f32));
            assert!(list.is_sorted(), "step {step}");
        }
        list.drain_events();

        for n in nodes.iter().take(3) {
            list.remove_entity(*n);
            assert!(list.is_sorted());
        }
        assert_eq!(list.entity_count(), 4);
        // Four entity nodes plus the witness AOI's two bound nodes.
        assert_eq!(list.iter_x().len(), 6);
        assert_eq!(list.iter_z().len(), 6);
    }

    /// The default constructor matches `new`.
    #[test]
    fn default_matches_new() {
        let list = RangeList::default();
        assert!(list.is_sorted());
        assert_eq!(list.entity_count(), 0);
        assert_eq!(list.trigger_count(), 0);
    }
    #[test]
    fn drain_events_for_keeps_other_triggers_queued() {
        let mut list = RangeList::new();
        let a = list.add_entity(1, v(0.0, 0.0));
        let b = list.add_entity(2, v(15.0, 0.0));
        let ta = list.add_aoi_trigger(a, 10.0);
        let tb = list.add_aoi_trigger(b, 10.0);
        list.trigger_insert(ta);
        list.trigger_insert(tb);
        list.drain_events();

        // a walks toward b: b enters a's widened AoI and a enters b's AoI.
        list.move_entity(a, v(12.0, 0.0));
        assert_eq!(entities(&list.drain_events_for(ta)), vec![2]);
        assert_eq!(entities(&list.drain_events_for(tb)), vec![1]);
        assert!(list.drain_events().is_empty());
    }
}
