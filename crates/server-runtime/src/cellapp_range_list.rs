//! The CellApp's area-of-interest core: the doubly-linked position lists
//! every entity and trigger bound lives on, and the trigger machinery that
//! fires enter/leave events when nodes cross each other.
//!
//! Reference faces:
//! - `server/cellapp/range_list_node.{hpp,cpp}` — [`RangeNode`]:
//!   the X and Z chains, [`RangeList::shuffle_x`]/[`RangeList::shuffle_z`],
//!   and the crossing dispatch.
//! - `server/cellapp/cell_range_list.{hpp,cpp}` — [`RangeList`]: the head
//!   and tail terminators and [`RangeList::add`].
//! - `server/cellapp/entity_range_list_node.cpp` — the entity node's
//!   flags and [`RangeList::entity_remove`].
//! - `server/cellapp/range_trigger.cpp` — the bound nodes and
//!   [`RangeTriggerData`]: insert/remove, the expand/contract shuffles and
//!   the crossedX/Z event faces.
//! - `server/cellapp/witness.cpp:73` (`AoITrigger`) and
//!   `range_list_appeal_trigger.cpp` — the two flag combinations the
//!   machinery serves: an observer's AoI trigger (wants AoI crossings)
//!   and a large entity's appeal trigger (makes them).
//!
//! The two chains share one arena of nodes, exactly as the reference's
//! four pointers per node do. Coordinates are read live — an entity node
//! from the entity's global position, a bound node from its trigger's
//! central node plus its offset — so the terminators' `±f32::MAX` and
//! the `isEqual` ties bubble exactly as the reference's `x()`/`z()`
//! virtuals do.

use std::cell::RefCell;
use std::fmt;
use std::rc::Rc;

use crate::cellapp_space::Entity;

/// `FLAG_NO_TRIGGERS`: a node that neither wants nor makes crossings.
pub const FLAG_NO_TRIGGERS: u8 = 0;
/// `FLAG_ENTITY_TRIGGER`: the bound wants to see plain entity nodes.
pub const FLAG_ENTITY_TRIGGER: u8 = 0x01;
/// `FLAG_LOWER_AOI_TRIGGER`: the bound wants lower-AoI-making nodes.
pub const FLAG_LOWER_AOI_TRIGGER: u8 = 0x02;
/// `FLAG_UPPER_AOI_TRIGGER`: the bound wants upper-AoI-making nodes.
pub const FLAG_UPPER_AOI_TRIGGER: u8 = 0x04;
/// `FLAG_IS_ENTITY`: the node makes crossings as an entity.
pub const FLAG_IS_ENTITY: u8 = 0x10;
/// `FLAG_IS_LOWER_BOUND`: the node is the lower bound of its trigger.
pub const FLAG_IS_LOWER_BOUND: u8 = 0x20;

/// `RANGE_LIST_ORDER_*` (`range_list_node.hpp`): the tie-break order of
/// co-located nodes.
pub const ORDER_HEAD: u16 = 0;
/// `RANGE_LIST_ORDER_ENTITY`.
pub const ORDER_ENTITY: u16 = 100;
/// `RANGE_LIST_ORDER_LOWER_BOUND`.
pub const ORDER_LOWER_BOUND: u16 = 190;
/// `RANGE_LIST_ORDER_UPPER_BOUND`.
pub const ORDER_UPPER_BOUND: u16 = 200;
/// `RANGE_LIST_ORDER_TAIL`.
pub const ORDER_TAIL: u16 = 0xffff;

/// An arena index into [`RangeList::nodes`].
pub type NodeId = usize;
/// An arena index into [`RangeList::triggers`].
pub type TriggerId = u32;

/// What a crossing event lands in — the reference's virtual
/// `triggerEnter`/`triggerLeave` pair. The recorder is the port's test
/// face; the witness's AoI trigger joins here when it ports.
#[derive(Clone)]
pub enum TriggerTarget {
    /// Every enter/leave appends `(entity, entering)`.
    Recorder(Rc<RefCell<Vec<(u32, bool)>>>),
}

/// One node of the range lists — the reference's `RangeListNode` and its
/// three bodies.
struct RangeNode {
    body: NodeBody,
    prev_x: Option<NodeId>,
    next_x: Option<NodeId>,
    prev_z: Option<NodeId>,
    next_z: Option<NodeId>,
    /// `wantsFlags_`: the crossings this node wants to receive.
    wants: u8,
    /// `makesFlags_`: the crossings this node makes others receive.
    makes: u8,
    /// `order_`: the co-location tie-break.
    order: u16,
}

/// The reference's `RangeListNode` bodies.
enum NodeBody {
    /// `RangeListTerminator`: head (`-f32::MAX`) or tail (`+f32::MAX`).
    Terminator,
    /// `EntityRangeListNode`: the entity's entry, its position read live
    /// from the entity.
    Entity {
        entity: Rc<RefCell<Entity>>,
        /// `isAoITrigger`: an entity with an appeal radius stops making
        /// AoI crossings itself — the appeal trigger takes over.
        is_aoi_trigger: bool,
    },
    /// `RangeTriggerNode`: one bound of a trigger, offset from the
    /// trigger's central node by `range`.
    Bound {
        trigger: TriggerId,
        range: f32,
        old_range: f32,
    },
}

/// `RangeTrigger` (`range_trigger.hpp`): the upper and lower bound pair
/// around one central node.
pub struct RangeTriggerData {
    central: NodeId,
    upper: NodeId,
    lower: NodeId,
    old_entity_x: f32,
    old_entity_z: f32,
    target: TriggerTarget,
}

/// `RangeList` (`cell_range_list.hpp`): both sorted chains through one
/// arena of nodes.
pub struct RangeList {
    nodes: Vec<Option<RangeNode>>,
    free_nodes: Vec<NodeId>,
    triggers: Vec<Option<RangeTriggerData>>,
    free_triggers: Vec<TriggerId>,
    head: NodeId,
    tail: NodeId,
}

impl Default for RangeList {
    /// The empty list — `Default` and `new` are the same wired pair.
    fn default() -> Self {
        Self::new()
    }
}

impl RangeList {
    /// The empty list: the head and tail terminators wired on both
    /// chains.
    pub fn new() -> Self {
        let mut list = RangeList {
            nodes: Vec::new(),
            free_nodes: Vec::new(),
            triggers: Vec::new(),
            free_triggers: Vec::new(),
            head: 0,
            tail: 0,
        };
        let head = list.alloc_node(RangeNode {
            body: NodeBody::Terminator,
            prev_x: None,
            next_x: None,
            prev_z: None,
            next_z: None,
            wants: FLAG_NO_TRIGGERS,
            makes: FLAG_NO_TRIGGERS,
            order: ORDER_HEAD,
        });
        let tail = list.alloc_node(RangeNode {
            body: NodeBody::Terminator,
            prev_x: None,
            next_x: None,
            prev_z: None,
            next_z: None,
            wants: FLAG_NO_TRIGGERS,
            makes: FLAG_NO_TRIGGERS,
            order: ORDER_TAIL,
        });
        list.head = head;
        list.tail = tail;
        // `last_.insertBeforeX( &first_ )` — the empty chain is
        // head <-> tail.
        list.node_mut(tail).prev_x = Some(head);
        list.node_mut(tail).prev_z = Some(head);
        list.node_mut(head).next_x = Some(tail);
        list.node_mut(head).next_z = Some(tail);
        list
    }

    fn alloc_node(&mut self, node: RangeNode) -> NodeId {
        if let Some(id) = self.free_nodes.pop() {
            self.nodes[id] = Some(node);
            id
        } else {
            self.nodes.push(Some(node));
            self.nodes.len() - 1
        }
    }

    fn node(&self, id: NodeId) -> &RangeNode {
        self.nodes[id].as_ref().expect("a live node")
    }

    fn node_mut(&mut self, id: NodeId) -> &mut RangeNode {
        self.nodes[id].as_mut().expect("a live node")
    }

    fn alloc_trigger(&mut self, data: RangeTriggerData) -> TriggerId {
        if let Some(id) = self.free_triggers.pop() {
            self.triggers[id as usize] = Some(data);
            id
        } else {
            self.triggers.push(Some(data));
            (self.triggers.len() - 1) as TriggerId
        }
    }

    /// The trigger of `tid`, if it is still live.
    pub fn trigger(&self, tid: TriggerId) -> Option<&RangeTriggerData> {
        self.triggers[tid as usize].as_ref()
    }

    /// The bound node's `(trigger, range, old_range)`, `None` for any
    /// other body. The tests drive the `None` arm directly; the call
    /// sites hold the bound invariant behind `expect`s.
    fn as_bound(&self, node: NodeId) -> Option<(TriggerId, f32, f32)> {
        match &self.node(node).body {
            NodeBody::Bound {
                trigger,
                range,
                old_range,
            } => Some((*trigger, *range, *old_range)),
            _ => None,
        }
    }

    /// The bound node's mutable `(trigger, range, old_range)` slots,
    /// `None` for any other body.
    fn bound_slots_mut(&mut self, node: NodeId) -> Option<(&mut TriggerId, &mut f32, &mut f32)> {
        match &mut self.nodes[node].as_mut().expect("a live node").body {
            NodeBody::Bound {
                trigger,
                range,
                old_range,
            } => Some((trigger, range, old_range)),
            _ => None,
        }
    }

    // ----- Coordinates -------------------------------------------------

    /// The node's live X — the entity's global position, the central
    /// node's X plus the bound's offset, or the terminator's `±MAX`.
    pub fn x(&self, id: NodeId) -> f32 {
        match &self.node(id).body {
            NodeBody::Terminator => {
                if self.node(id).order == ORDER_TAIL {
                    f32::MAX
                } else {
                    -f32::MAX
                }
            }
            NodeBody::Entity { entity, .. } => entity.borrow().position[0],
            NodeBody::Bound { trigger, range, .. } => {
                // The reference's `volatile` keeps the sum in f32; the
                // port's f32 arithmetic does too.
                let central = self.trigger(*trigger).expect("live trigger").central;
                self.x(central) + *range
            }
        }
    }

    /// The node's live Z.
    pub fn z(&self, id: NodeId) -> f32 {
        match &self.node(id).body {
            NodeBody::Terminator => {
                if self.node(id).order == ORDER_TAIL {
                    f32::MAX
                } else {
                    -f32::MAX
                }
            }
            NodeBody::Entity { entity, .. } => entity.borrow().position[2],
            NodeBody::Bound { trigger, range, .. } => {
                let central = self.trigger(*trigger).expect("live trigger").central;
                self.z(central) + *range
            }
        }
    }

    /// The bound node's old X (`oldX`): the trigger's old central
    /// position plus its old offset.
    fn old_x(&self, id: NodeId) -> f32 {
        let (trigger, _, old_range) = self.as_bound(id).expect("the old positions live on bounds");
        let data = self.trigger(trigger).expect("live trigger");
        data.old_entity_x + old_range
    }

    /// The bound node's old Z (`oldZ`).
    fn old_z(&self, id: NodeId) -> f32 {
        let (trigger, _, old_range) = self.as_bound(id).expect("the old positions live on bounds");
        let data = self.trigger(trigger).expect("live trigger");
        data.old_entity_z + old_range
    }

    fn wants_crossing(&self, a: NodeId, b: NodeId) -> bool {
        self.node(a).wants & self.node(b).makes != 0
    }

    // ----- Membership --------------------------------------------------

    /// `RangeList::add`: the node enters right after the head on both
    /// chains, then shuffles out from `-MAX` — the insertion shuffle
    /// fires every enter it passes.
    pub fn add(&mut self, node: NodeId) {
        let after_head_x = self.node(self.head).next_x.expect("tail");
        let after_head_z = self.node(self.head).next_z.expect("tail");
        self.insert_before_x(after_head_x, node);
        self.insert_before_z(after_head_z, node);
        self.shuffle_x_then_z(node, -f32::MAX, -f32::MAX);
    }

    /// `RangeListNode::removeFromRangeList`. Every live node is flanked
    /// by the terminators, so all four neighbours exist.
    pub fn remove_from_range_list(&mut self, node: NodeId) {
        let (prev_x, next_x, prev_z, next_z) = {
            let n = self.node(node);
            (n.prev_x, n.next_x, n.prev_z, n.next_z)
        };
        self.node_mut(prev_x.expect("a flanked node")).next_x = next_x;
        self.node_mut(next_x.expect("a flanked node")).prev_x = prev_x;
        self.node_mut(prev_z.expect("a flanked node")).next_z = next_z;
        self.node_mut(next_z.expect("a flanked node")).prev_z = prev_z;
        self.nodes[node] = None;
        self.free_nodes.push(node);
    }

    /// `RangeListNode::insertBeforeX`: `node` takes `this`'s place in the
    /// X chain.
    pub fn insert_before_x(&mut self, this: NodeId, node: NodeId) {
        debug_assert!(this != node);
        // Even the tail has the head as its predecessor.
        let prev = self.node(this).prev_x.expect("a flanked node");
        self.node_mut(prev).next_x = Some(node);
        self.node_mut(node).prev_x = Some(prev);
        self.node_mut(this).prev_x = Some(node);
        self.node_mut(node).next_x = Some(this);
    }

    /// `RangeListNode::insertBeforeZ`.
    pub fn insert_before_z(&mut self, this: NodeId, node: NodeId) {
        debug_assert!(this != node);
        let prev = self.node(this).prev_z.expect("a flanked node");
        self.node_mut(prev).next_z = Some(node);
        self.node_mut(node).prev_z = Some(prev);
        self.node_mut(this).prev_z = Some(node);
        self.node_mut(node).next_z = Some(this);
    }

    // ----- The shuffles ------------------------------------------------

    /// `RangeListNode::shuffleXThenZ`.
    pub fn shuffle_x_then_z(&mut self, node: NodeId, old_x: f32, old_z: f32) {
        self.shuffle_x(node, old_x, old_z);
        self.shuffle_z(node, old_x, old_z);
    }

    /// `RangeListNode::shuffleX`: bubble the node towards its position,
    /// firing `crossedX` on both sides of every swap.
    pub fn shuffle_x(&mut self, node: NodeId, old_x: f32, old_z: f32) {
        let our_pos_x = self.x(node);
        // Shuffle to the left (negative X)...
        while let Some(prev) = self.node(node).prev_x {
            let oth_pos_x = self.x(prev);
            let passes = our_pos_x < oth_pos_x
                || (our_pos_x == oth_pos_x && self.node(node).order <= self.node(prev).order);
            if !passes {
                break;
            }
            if self.wants_crossing(node, prev) {
                self.crossed_x(node, prev, true, oth_pos_x, self.z(prev));
            }
            if self.wants_crossing(prev, node) {
                self.crossed_x(prev, node, false, old_x, old_z);
            }
            self.shift_left_x(node);
        }
        // Shuffle to the right (positive X)...
        while let Some(next) = self.node(node).next_x {
            let oth_pos_x = self.x(next);
            let passes = our_pos_x > oth_pos_x
                || (our_pos_x == oth_pos_x && self.node(node).order >= self.node(next).order);
            if !passes {
                break;
            }
            if self.wants_crossing(node, next) {
                self.crossed_x(node, next, false, oth_pos_x, self.z(next));
            }
            if self.wants_crossing(next, node) {
                self.crossed_x(next, node, true, old_x, old_z);
            }
            self.shift_right_x(node);
        }
    }

    /// `RangeListNode::shuffleZ`.
    pub fn shuffle_z(&mut self, node: NodeId, old_x: f32, old_z: f32) {
        let our_pos_z = self.z(node);
        while let Some(prev) = self.node(node).prev_z {
            let oth_pos_z = self.z(prev);
            let passes = our_pos_z < oth_pos_z
                || (our_pos_z == oth_pos_z && self.node(node).order <= self.node(prev).order);
            if !passes {
                break;
            }
            if self.wants_crossing(node, prev) {
                self.crossed_z(node, prev, true, self.x(prev), oth_pos_z);
            }
            if self.wants_crossing(prev, node) {
                self.crossed_z(prev, node, false, old_x, old_z);
            }
            self.shift_left_z(node);
        }
        while let Some(next) = self.node(node).next_z {
            let oth_pos_z = self.z(next);
            let passes = our_pos_z > oth_pos_z
                || (our_pos_z == oth_pos_z && self.node(node).order >= self.node(next).order);
            if !passes {
                break;
            }
            if self.wants_crossing(node, next) {
                self.crossed_z(node, next, false, self.x(next), oth_pos_z);
            }
            if self.wants_crossing(next, node) {
                self.crossed_z(next, node, true, old_x, old_z);
            }
            self.shift_right_z(node);
        }
    }

    /// The left half of `shuffleX`'s pointer surgery, one step.
    fn shift_left_x(&mut self, node: NodeId) {
        let prev = self.node(node).prev_x.expect("the loop guard");
        let next = self.node(node).next_x.expect("a flanked node");
        self.node_mut(next).prev_x = Some(prev);
        self.node_mut(prev).next_x = Some(next);
        // The shuffle never passes the head terminator (its position is
        // -MAX and every real order outranks ORDER_HEAD), so `prev` has
        // a predecessor too.
        let grand = self.node(prev).prev_x.expect("a flanked node");
        self.node_mut(node).next_x = Some(prev);
        self.node_mut(node).prev_x = Some(grand);
        self.node_mut(grand).next_x = Some(node);
        self.node_mut(prev).prev_x = Some(node);
    }

    /// The right half of `shuffleX`'s pointer surgery, one step.
    fn shift_right_x(&mut self, node: NodeId) {
        let next = self.node(node).next_x.expect("the loop guard");
        let prev = self.node(node).prev_x.expect("a flanked node");
        self.node_mut(prev).next_x = Some(next);
        self.node_mut(next).prev_x = Some(prev);
        // The loop never passes the tail terminator (its position is
        // MAX), so `next` has a successor too.
        let beyond = self.node(next).next_x.expect("a flanked node");
        self.node_mut(node).prev_x = Some(next);
        self.node_mut(node).next_x = Some(beyond);
        self.node_mut(next).next_x = Some(node);
        self.node_mut(beyond).prev_x = Some(node);
    }

    /// The left half of `shuffleZ`'s pointer surgery, one step.
    fn shift_left_z(&mut self, node: NodeId) {
        let prev = self.node(node).prev_z.expect("the loop guard");
        let next = self.node(node).next_z.expect("a flanked node");
        self.node_mut(next).prev_z = Some(prev);
        self.node_mut(prev).next_z = Some(next);
        let grand = self.node(prev).prev_z.expect("a flanked node");
        self.node_mut(node).next_z = Some(prev);
        self.node_mut(node).prev_z = Some(grand);
        self.node_mut(grand).next_z = Some(node);
        self.node_mut(prev).prev_z = Some(node);
    }

    /// The right half of `shuffleZ`'s pointer surgery, one step.
    fn shift_right_z(&mut self, node: NodeId) {
        let next = self.node(node).next_z.expect("the loop guard");
        let prev = self.node(node).prev_z.expect("a flanked node");
        self.node_mut(prev).next_z = Some(next);
        self.node_mut(next).prev_z = Some(prev);
        let beyond = self.node(next).next_z.expect("a flanked node");
        self.node_mut(node).prev_z = Some(next);
        self.node_mut(node).next_z = Some(beyond);
        self.node_mut(next).next_z = Some(node);
        self.node_mut(beyond).prev_z = Some(node);
    }

    // ----- The crossing dispatch --------------------------------------

    /// `RangeTriggerNode::crossedX`: only trigger bounds receive
    /// crossings — the entity node's base face is empty.
    fn crossed_x(
        &mut self,
        node: NodeId,
        other: NodeId,
        positive: bool,
        old_oth_x: f32,
        old_oth_z: f32,
    ) {
        // Only bound nodes receive crossings (the wants gate).
        let (trigger, range, old_range) = self.as_bound(node).expect("the crossing is a bound");
        let is_lower = self.node(node).makes & FLAG_IS_LOWER_BOUND != 0;
        let other_entity = self.entity_of(other);
        if let Some(other_entity) = other_entity {
            self.crossed_x_entity(
                node,
                trigger,
                range,
                old_range,
                is_lower,
                other,
                other_entity,
                positive,
                old_oth_x,
                old_oth_z,
            );
        } else {
            self.crossed_x_entity_range(node, trigger, range, old_range, is_lower, other, positive);
        }
    }

    /// `RangeTriggerNode::crossedZ`.
    fn crossed_z(
        &mut self,
        node: NodeId,
        other: NodeId,
        positive: bool,
        old_oth_x: f32,
        old_oth_z: f32,
    ) {
        // Only bound nodes receive crossings (the wants gate).
        let (trigger, range, old_range) = self.as_bound(node).expect("the crossing is a bound");
        let is_lower = self.node(node).makes & FLAG_IS_LOWER_BOUND != 0;
        let other_entity = self.entity_of(other);
        if let Some(other_entity) = other_entity {
            self.crossed_z_entity(
                node,
                trigger,
                range,
                old_range,
                is_lower,
                other,
                other_entity,
                positive,
                old_oth_x,
                old_oth_z,
            );
        } else {
            self.crossed_z_entity_range(node, trigger, range, old_range, is_lower, other, positive);
        }
    }

    /// The entity a node carries, when it is an entity node.
    fn entity_of(&self, node: NodeId) -> Option<Rc<RefCell<Entity>>> {
        match &self.node(node).body {
            NodeBody::Entity { entity, .. } => Some(Rc::clone(entity)),
            _ => None,
        }
    }

    /// `RangeTriggerNode::crossedXEntity`.
    #[allow(clippy::too_many_arguments)]
    fn crossed_x_entity(
        &mut self,
        _bound: NodeId,
        trigger: TriggerId,
        range: f32,
        old_range: f32,
        is_lower: bool,
        other: NodeId,
        other_entity: Rc<RefCell<Entity>>,
        positive: bool,
        old_oth_x: f32,
        old_oth_z: f32,
    ) {
        let central = self.trigger(trigger).expect("live trigger").central;
        if other == central {
            return;
        }
        // X is shuffled first, so the old Z position is the gate.
        let was_in_z = self.was_in_z_range(trigger, old_oth_z, old_range.abs());
        if !was_in_z {
            return;
        }
        let is_entering = is_lower == positive;
        if is_entering {
            let is_in_x = self.is_in_x_range(trigger, self.x(other), range.abs());
            let is_in_z = self.is_in_z_range(trigger, self.z(other), range.abs());
            if is_in_x && is_in_z {
                self.trigger_enter(trigger, &other_entity);
            }
        } else {
            let was_in_x = self.was_in_x_range(trigger, old_oth_x, old_range.abs());
            if was_in_x {
                self.trigger_leave(trigger, &other_entity);
            }
        }
    }

    /// `RangeTriggerNode::crossedXEntityRange` — this bound crossed
    /// another trigger's bound (a large entity's appeal).
    #[allow(clippy::too_many_arguments)]
    fn crossed_x_entity_range(
        &mut self,
        _bound: NodeId,
        trigger: TriggerId,
        range: f32,
        old_range: f32,
        is_lower: bool,
        other: NodeId,
        positive: bool,
    ) {
        // The other side of a bound-vs-bound crossing is a bound.
        let (other_trigger, _, _) = self.as_bound(other).expect("the other is a bound");
        let (other_lower, other_upper, central) = {
            let data = self.trigger(other_trigger).expect("live trigger");
            (data.lower, data.upper, data.central)
        };
        // The reference dereferences `otherRangeTrigger.pEntity()`
        // unchecked: a trigger's central is always an entity node.
        let other_entity = self
            .entity_of(central)
            .expect("a trigger's central is an entity node");
        // X is shuffled first, so the old Z span is the gate.
        let was_in_z = self.was_in_z_span(
            trigger,
            old_range.abs(),
            self.old_z(other_lower),
            self.old_z(other_upper),
        );
        if !was_in_z {
            return;
        }
        let is_entering = is_lower == positive;
        if is_entering {
            let is_in_x = self.is_in_x_span(
                trigger,
                range.abs(),
                self.x(other_lower),
                self.x(other_upper),
            );
            let is_in_z = self.is_in_z_span(
                trigger,
                range.abs(),
                self.z(other_lower),
                self.z(other_upper),
            );
            if is_in_x && is_in_z {
                self.trigger_enter(trigger, &other_entity);
            }
            return;
        }
        let was_in_x = self.was_in_x_span(
            trigger,
            old_range.abs(),
            self.old_x(other_lower),
            self.old_x(other_upper),
        );
        if !was_in_x {
            return;
        }
        self.trigger_leave(trigger, &other_entity);
    }

    /// `RangeTriggerNode::crossedZEntity`.
    #[allow(clippy::too_many_arguments)]
    fn crossed_z_entity(
        &mut self,
        _bound: NodeId,
        trigger: TriggerId,
        range: f32,
        old_range: f32,
        is_lower: bool,
        other: NodeId,
        other_entity: Rc<RefCell<Entity>>,
        positive: bool,
        old_oth_x: f32,
        old_oth_z: f32,
    ) {
        let central = self.trigger(trigger).expect("live trigger").central;
        if other == central {
            return;
        }
        // Z is shuffled second, so the new X position is the gate.
        let is_in_x = self.is_in_x_range(trigger, self.x(other), range.abs());
        if !is_in_x {
            return;
        }
        let is_entering = is_lower == positive;
        if is_entering {
            let is_in_z = self.is_in_z_range(trigger, self.z(other), range.abs());
            if is_in_z {
                self.trigger_enter(trigger, &other_entity);
            }
        } else {
            let was_in_x = self.was_in_x_range(trigger, old_oth_x, old_range.abs());
            let was_in_z = self.was_in_z_range(trigger, old_oth_z, old_range.abs());
            if was_in_x && was_in_z {
                self.trigger_leave(trigger, &other_entity);
            }
        }
    }

    /// `RangeTriggerNode::crossedZEntityRange`.
    #[allow(clippy::too_many_arguments)]
    fn crossed_z_entity_range(
        &mut self,
        _bound: NodeId,
        trigger: TriggerId,
        range: f32,
        old_range: f32,
        is_lower: bool,
        other: NodeId,
        positive: bool,
    ) {
        // The other side of a bound-vs-bound crossing is a bound.
        let (other_trigger, _, _) = self.as_bound(other).expect("the other is a bound");
        let (other_lower, other_upper, central) = {
            let data = self.trigger(other_trigger).expect("live trigger");
            (data.lower, data.upper, data.central)
        };
        // The reference dereferences `otherRangeTrigger.pEntity()`
        // unchecked: a trigger's central is always an entity node.
        let other_entity = self
            .entity_of(central)
            .expect("a trigger's central is an entity node");
        // Z is shuffled second, so the new X span is the gate.
        let is_in_x = self.is_in_x_span(
            trigger,
            range.abs(),
            self.x(other_lower),
            self.x(other_upper),
        );
        if !is_in_x {
            return;
        }
        let is_entering = is_lower == positive;
        if is_entering {
            let is_in_z = self.is_in_z_span(
                trigger,
                range.abs(),
                self.z(other_lower),
                self.z(other_upper),
            );
            if is_in_z {
                self.trigger_enter(trigger, &other_entity);
            }
            return;
        }
        let was_in_x = self.was_in_x_span(
            trigger,
            old_range.abs(),
            self.old_x(other_lower),
            self.old_x(other_upper),
        );
        let was_in_z = self.was_in_z_span(
            trigger,
            old_range.abs(),
            self.old_z(other_lower),
            self.old_z(other_upper),
        );
        if was_in_x && was_in_z {
            self.trigger_leave(trigger, &other_entity);
        }
    }

    // ----- The range predicates ---------------------------------------

    /// `RangeTrigger::isInXRange(x, range)`: `(sub - range) < x` and
    /// `x <= sub + range` — the lower bound is exclusive, the upper
    /// inclusive.
    pub fn is_in_x_range(&self, trigger: TriggerId, x: f32, range: f32) -> bool {
        let sub_x = self.trigger(trigger).expect("live trigger").central;
        let sub_x = self.x(sub_x);
        let lower_bound = sub_x - range;
        let upper_bound = sub_x + range;
        (lower_bound < x) && (x <= upper_bound)
    }

    /// `RangeTrigger::wasInXRange(x, range)`.
    pub fn was_in_x_range(&self, trigger: TriggerId, x: f32, range: f32) -> bool {
        let sub_x = self.trigger(trigger).expect("live trigger").old_entity_x;
        let lower_bound = sub_x - range;
        let upper_bound = sub_x + range;
        (lower_bound < x) && (x <= upper_bound)
    }

    /// `RangeTrigger::isInZRange(z, range)`.
    pub fn is_in_z_range(&self, trigger: TriggerId, z: f32, range: f32) -> bool {
        let sub_z = self.trigger(trigger).expect("live trigger").central;
        let sub_z = self.z(sub_z);
        let lower_bound = sub_z - range;
        let upper_bound = sub_z + range;
        (lower_bound < z) && (z <= upper_bound)
    }

    /// `RangeTrigger::wasInZRange(z, range)`.
    pub fn was_in_z_range(&self, trigger: TriggerId, z: f32, range: f32) -> bool {
        let sub_z = self.trigger(trigger).expect("live trigger").old_entity_z;
        let lower_bound = sub_z - range;
        let upper_bound = sub_z + range;
        (lower_bound < z) && (z <= upper_bound)
    }

    /// `RangeTrigger::isInXRange(entityLower, entityUpper, range)`: the
    /// span-overlap form a bound-vs-bound crossing checks.
    pub fn is_in_x_span(
        &self,
        trigger: TriggerId,
        range: f32,
        entity_lower: f32,
        entity_upper: f32,
    ) -> bool {
        let sub_x = self.trigger(trigger).expect("live trigger").central;
        let sub_x = self.x(sub_x);
        let lower_bound = sub_x - range;
        let upper_bound = sub_x + range;
        (lower_bound <= entity_upper) && (entity_lower <= upper_bound)
    }

    /// `RangeTrigger::wasInXRange(entityLower, entityUpper, range)`.
    pub fn was_in_x_span(
        &self,
        trigger: TriggerId,
        range: f32,
        entity_lower: f32,
        entity_upper: f32,
    ) -> bool {
        let sub_x = self.trigger(trigger).expect("live trigger").old_entity_x;
        let lower_bound = sub_x - range;
        let upper_bound = sub_x + range;
        (lower_bound <= entity_upper) && (entity_lower <= upper_bound)
    }

    /// `RangeTrigger::isInZRange(entityLower, entityUpper, range)`.
    pub fn is_in_z_span(
        &self,
        trigger: TriggerId,
        range: f32,
        entity_lower: f32,
        entity_upper: f32,
    ) -> bool {
        let sub_z = self.trigger(trigger).expect("live trigger").central;
        let sub_z = self.z(sub_z);
        let lower_bound = sub_z - range;
        let upper_bound = sub_z + range;
        (lower_bound <= entity_upper) && (entity_lower <= upper_bound)
    }

    /// `RangeTrigger::wasInZRange(entityLower, entityUpper, range)`.
    pub fn was_in_z_span(
        &self,
        trigger: TriggerId,
        range: f32,
        entity_lower: f32,
        entity_upper: f32,
    ) -> bool {
        let sub_z = self.trigger(trigger).expect("live trigger").old_entity_z;
        let lower_bound = sub_z - range;
        let upper_bound = sub_z + range;
        (lower_bound <= entity_upper) && (entity_lower <= upper_bound)
    }

    /// The trigger's radius (`range()`).
    pub fn trigger_range(&self, trigger: TriggerId) -> f32 {
        let upper = self.trigger(trigger).expect("live trigger").upper;
        self.as_bound(upper).expect("the bound body").1
    }

    /// `RangeTrigger::contains(node)`: the node sits inside the trigger
    /// on both axes, ties broken by order.
    pub fn trigger_contains(&self, trigger: TriggerId, node: NodeId) -> bool {
        let data = self.trigger(trigger).expect("live trigger");
        let (qx, qz, qo) = (self.x(node), self.z(node), self.node(node).order);
        let (lx, lz, lo) = (
            self.x(data.lower),
            self.z(data.lower),
            self.node(data.lower).order,
        );
        let (ux, uz, uo) = (
            self.x(data.upper),
            self.z(data.upper),
            self.node(data.upper).order,
        );
        (lx < qx || (lx == qx && lo < qo))
            && (qx < ux || (qx == ux && qo < uo))
            && (lz < qz || (lz == qz && lo < qo))
            && (qz < uz || (qz == uz && qo < uo))
    }

    /// `RangeTrigger::containsInZ(node)`.
    pub fn trigger_contains_in_z(&self, trigger: TriggerId, node: NodeId) -> bool {
        let data = self.trigger(trigger).expect("live trigger");
        let (qz, qo) = (self.z(node), self.node(node).order);
        let (lz, lo) = (self.z(data.lower), self.node(data.lower).order);
        let (uz, uo) = (self.z(data.upper), self.node(data.upper).order);
        (lz < qz || (lz == qz && lo < qo)) && (qz < uz || (qz == uz && qo < uo))
    }

    // ----- The event dispatch -----------------------------------------

    fn trigger_enter(&mut self, trigger: TriggerId, entity: &Rc<RefCell<Entity>>) {
        let id = entity.borrow().id;
        let TriggerTarget::Recorder(events) = &self.triggers[trigger as usize]
            .as_ref()
            .expect("live trigger")
            .target;
        events.borrow_mut().push((id, true));
    }

    fn trigger_leave(&mut self, trigger: TriggerId, entity: &Rc<RefCell<Entity>>) {
        let id = entity.borrow().id;
        let TriggerTarget::Recorder(events) = &self.triggers[trigger as usize]
            .as_ref()
            .expect("live trigger")
            .target;
        events.borrow_mut().push((id, false));
    }

    // ----- The entity faces -------------------------------------------

    /// `EntityRangeListNode`'s constructor plus `RangeList::add`: the
    /// entity node enters the list, its insertion shuffle firing every
    /// enter it passes.
    pub fn add_entity_node(&mut self, entity: Rc<RefCell<Entity>>) -> NodeId {
        let node = self.alloc_node(RangeNode {
            body: NodeBody::Entity {
                entity,
                is_aoi_trigger: true,
            },
            prev_x: None,
            next_x: None,
            prev_z: None,
            next_z: None,
            wants: FLAG_NO_TRIGGERS,
            makes: FLAG_ENTITY_TRIGGER
                | FLAG_LOWER_AOI_TRIGGER
                | FLAG_UPPER_AOI_TRIGGER
                | FLAG_IS_ENTITY,
            order: ORDER_ENTITY,
        });
        self.add(node);
        node
    }

    /// `EntityRangeListNode::isAoITrigger`: an entity under an appeal
    /// trigger stops making AoI crossings itself — the appeal trigger's
    /// bounds take over.
    pub fn set_entity_aoi_trigger(&mut self, node: NodeId, is_aoi_trigger: bool) {
        let slot = self.nodes[node].as_mut().expect("a live node");
        let changing = FLAG_LOWER_AOI_TRIGGER | FLAG_UPPER_AOI_TRIGGER;
        match &mut slot.body {
            NodeBody::Entity {
                is_aoi_trigger: flag,
                ..
            } => {
                *flag = is_aoi_trigger;
                slot.makes = if is_aoi_trigger {
                    slot.makes | changing
                } else {
                    slot.makes & !changing
                };
            }
            // Only entity nodes carry the AoI witness flag.
            NodeBody::Bound { .. } | NodeBody::Terminator => {}
        }
    }

    /// `EntityRangeListNode::remove`: the entity moves to the maximum Z
    /// (firing every leave it passes), then leaves the list.
    pub fn entity_remove(&mut self, node: NodeId) {
        let old_z = self.z(node);
        let x = self.x(node);
        let entity = self.entity_of(node).expect("an entity node");
        entity.borrow_mut().position[2] = f32::MAX;
        self.shuffle_z(node, x, old_z);
        self.remove_from_range_list(node);
    }

    /// `Entity::updateInternalsForNewPosition`'s shuffle body: the
    /// leading triggers expand, the entity node shuffles, the trailing
    /// triggers contract — the reference's order for a move that does
    /// not lose crossings mid-flight.
    pub fn shuffle_entity(&mut self, node: NodeId, triggers: &[TriggerId], old_x: f32, old_z: f32) {
        let increase_x = old_x < self.x(node);
        let increase_z = old_z < self.z(node);
        for &trigger in triggers {
            self.trigger_shuffle_expand(trigger, increase_x, increase_z, old_x, old_z);
        }
        self.shuffle_x_then_z(node, old_x, old_z);
        for &trigger in triggers.iter().rev() {
            self.trigger_shuffle_contract(trigger, increase_x, increase_z, old_x, old_z);
        }
    }

    /// `Entity::addTrigger`: the entity's triggers stay sorted by range,
    /// largest first, so the leading bound of the outermost trigger
    /// moves first.
    pub fn entity_add_trigger(&self, entity: &Rc<RefCell<Entity>>, trigger: TriggerId) {
        let range = self.trigger_range(trigger);
        let mut borrowed = entity.borrow_mut();
        let triggers = &mut borrowed.range_triggers;
        let index = triggers
            .iter()
            .position(|&existing| self.trigger_range(existing) <= range)
            .unwrap_or(triggers.len());
        triggers.insert(index, trigger);
    }

    /// `Entity::delTrigger`.
    pub fn entity_del_trigger(&self, entity: &Rc<RefCell<Entity>>, trigger: TriggerId) {
        let mut borrowed = entity.borrow_mut();
        let triggers = &mut borrowed.range_triggers;
        if let Some(index) = triggers.iter().position(|&existing| existing == trigger) {
            triggers.remove(index);
        }
    }

    // ----- The trigger faces ------------------------------------------

    /// `RangeTrigger`'s constructor: the central node and its two
    /// bounds, with the witness's AoI observer flag shape by default.
    /// The argument count is the reference ctor's.
    #[allow(clippy::too_many_arguments)]
    pub fn create_trigger(
        &mut self,
        central: NodeId,
        range: f32,
        wants_lower: u8,
        wants_upper: u8,
        makes_lower: u8,
        makes_upper: u8,
        target: TriggerTarget,
    ) -> TriggerId {
        let upper = self.alloc_node(RangeNode {
            body: NodeBody::Bound {
                trigger: 0,
                range,
                old_range: 0.0,
            },
            prev_x: None,
            next_x: None,
            prev_z: None,
            next_z: None,
            wants: wants_upper,
            makes: makes_upper,
            order: ORDER_UPPER_BOUND,
        });
        let lower = self.alloc_node(RangeNode {
            body: NodeBody::Bound {
                trigger: 0,
                range: -range,
                old_range: 0.0,
            },
            prev_x: None,
            next_x: None,
            prev_z: None,
            next_z: None,
            wants: wants_lower,
            makes: makes_lower | FLAG_IS_LOWER_BOUND,
            order: ORDER_LOWER_BOUND,
        });
        let data = RangeTriggerData {
            central,
            upper,
            lower,
            old_entity_x: self.x(central),
            old_entity_z: self.z(central),
            target,
        };
        let trigger = self.alloc_trigger(data);
        for bound in [upper, lower] {
            *self.bound_slots_mut(bound).expect("the bound body").0 = trigger;
        }
        trigger
    }

    /// `RangeTrigger::insert`: place both bounds just past any co-located
    /// lower-order nodes, then shuffle out from a point — the initial
    /// expansion fires an enter for everything already inside. The
    /// reference's co-location comment: inserting naively at the central
    /// node's neighbours would fire spurious leaves for nodes that
    /// happen to sort after the central one.
    pub fn trigger_insert(&mut self, trigger: TriggerId) {
        let (central, lower, upper) = {
            let data = self.trigger(trigger).expect("live trigger");
            (data.central, data.lower, data.upper)
        };
        let upper_order = self.node(upper).order;
        debug_assert!(
            self.node(lower).order < upper_order
                && self.node(lower).order > self.node(central).order
                && upper_order > self.node(central).order
        );

        let mut cursor = self.node(central).next_x.expect("tail");
        while self.x(cursor) == self.x(central) && self.node(cursor).order < upper_order {
            cursor = self.node(cursor).next_x.expect("tail");
        }
        self.insert_before_x(cursor, lower);
        self.insert_before_x(cursor, upper);

        let mut cursor = self.node(central).next_z.expect("tail");
        while self.z(cursor) == self.z(central) && self.node(cursor).order < upper_order {
            cursor = self.node(cursor).next_z.expect("tail");
        }
        self.insert_before_z(cursor, lower);
        self.insert_before_z(cursor, upper);

        let old_entity_x = self.x(central);
        let old_entity_z = self.z(central);
        {
            let data = self.triggers[trigger as usize]
                .as_mut()
                .expect("live trigger");
            data.old_entity_x = old_entity_x;
            data.old_entity_z = old_entity_z;
        }
        for bound in [upper, lower] {
            *self.bound_slots_mut(bound).expect("the bound body").2 = 0.0;
        }

        self.shuffle_x(upper, old_entity_x, old_entity_z);
        self.shuffle_x(lower, old_entity_x, old_entity_z);
        self.shuffle_z(upper, old_entity_x, old_entity_z);
        self.shuffle_z(lower, old_entity_x, old_entity_z);

        // Close the old-range window: `oldRange_ = range_`.
        for bound in [upper, lower] {
            let (_, range_slot, old_range_slot) =
                self.bound_slots_mut(bound).expect("the bound body");
            *old_range_slot = *range_slot;
        }
    }

    /// `RangeTrigger::remove`: contract to a point first (firing a leave
    /// for everything inside), then unlink. The reference restores the
    /// bounds' ranges after the unlink for a possible re-insert; the
    /// port's arena frees the slots with the trigger, so the restore
    /// happens before it, with the same observable face.
    pub fn trigger_remove(&mut self, trigger: TriggerId) {
        let (upper, lower) = {
            let data = self.trigger(trigger).expect("live trigger");
            (data.upper, data.lower)
        };
        let our = self.trigger_range(trigger);
        let (_, olr, _) = self.as_bound(lower).expect("the bound body");
        self.set_range(upper, 0.0);
        self.set_range(lower, 0.0);
        for (bound, range) in [(upper, our), (lower, olr)] {
            *self.bound_slots_mut(bound).expect("the bound body").1 = range;
        }
        self.trigger_remove_without_contracting(trigger);
    }

    /// `RangeTrigger::removeWithoutContracting`.
    pub fn trigger_remove_without_contracting(&mut self, trigger: TriggerId) {
        let (upper, lower) = {
            let data = self.trigger(trigger).expect("live trigger");
            (data.upper, data.lower)
        };
        self.remove_from_range_list(upper);
        self.remove_from_range_list(lower);
        self.triggers[trigger as usize] = None;
        self.free_triggers.push(trigger);
    }

    /// `RangeTrigger::shuffleXThenZ`.
    pub fn trigger_shuffle_x_then_z(&mut self, trigger: TriggerId, old_x: f32, old_z: f32) {
        let (central, upper, lower) = {
            let data = self.trigger(trigger).expect("live trigger");
            (data.central, data.upper, data.lower)
        };
        // "TODO: Remove this assert after a while" — the caller passes
        // the same old central position the trigger stored.
        debug_assert!(self.trigger(trigger).expect("live trigger").old_entity_x == old_x);
        debug_assert!(self.trigger(trigger).expect("live trigger").old_entity_z == old_z);
        let upper_range = self.as_bound(upper).expect("the bound body").1;
        let lower_range = self.as_bound(lower).expect("the bound body").1;
        self.shuffle_x_then_z(upper, old_x + upper_range, old_z + upper_range);
        self.shuffle_x_then_z(lower, old_x + lower_range, old_z + lower_range);
        let (cx, cz) = (self.x(central), self.z(central));
        let data = self.triggers[trigger as usize]
            .as_mut()
            .expect("live trigger");
        data.old_entity_x = cx;
        data.old_entity_z = cz;
    }

    /// `RangeTrigger::shuffleXThenZExpand`: only the bound that widens
    /// the area moves, so no crossings are lost on the shrinking side.
    pub fn trigger_shuffle_expand(
        &mut self,
        trigger: TriggerId,
        x_inc: bool,
        z_inc: bool,
        old_x: f32,
        old_z: f32,
    ) {
        let (upper, lower) = {
            let data = self.trigger(trigger).expect("live trigger");
            (data.upper, data.lower)
        };
        debug_assert!(self.trigger(trigger).expect("live trigger").old_entity_x == old_x);
        debug_assert!(self.trigger(trigger).expect("live trigger").old_entity_z == old_z);
        let x_trigger = if x_inc { upper } else { lower };
        let x_range = self.as_bound(x_trigger).expect("the bound body").1;
        self.shuffle_x(x_trigger, old_x + x_range, old_z + x_range);
        let z_trigger = if z_inc { upper } else { lower };
        let z_range = self.as_bound(z_trigger).expect("the bound body").1;
        self.shuffle_z(z_trigger, old_x + z_range, old_z + z_range);
    }

    /// `RangeTrigger::shuffleXThenZContract`: only the bound that
    /// narrows the area moves, then the old central position catches up.
    pub fn trigger_shuffle_contract(
        &mut self,
        trigger: TriggerId,
        x_inc: bool,
        z_inc: bool,
        old_x: f32,
        old_z: f32,
    ) {
        let (central, upper, lower) = {
            let data = self.trigger(trigger).expect("live trigger");
            (data.central, data.upper, data.lower)
        };
        debug_assert!(self.trigger(trigger).expect("live trigger").old_entity_x == old_x);
        debug_assert!(self.trigger(trigger).expect("live trigger").old_entity_z == old_z);
        let x_trigger = if x_inc { lower } else { upper };
        let x_range = self.as_bound(x_trigger).expect("the bound body").1;
        self.shuffle_x(x_trigger, old_x + x_range, old_z + x_range);
        let z_trigger = if z_inc { lower } else { upper };
        let z_range = self.as_bound(z_trigger).expect("the bound body").1;
        self.shuffle_z(z_trigger, old_x + z_range, old_z + z_range);
        let (cx, cz) = (self.x(central), self.z(central));
        let data = self.triggers[trigger as usize]
            .as_mut()
            .expect("live trigger");
        data.old_entity_x = cx;
        data.old_entity_z = cz;
    }

    /// `RangeTrigger::setRange`: floors at 1mm, then moves both bounds.
    pub fn trigger_set_range(&mut self, trigger: TriggerId, range: f32) {
        let r = if range < 0.001 { 0.001 } else { range };
        let (upper, lower) = {
            let data = self.trigger(trigger).expect("live trigger");
            (data.upper, data.lower)
        };
        self.set_range(upper, r);
        self.set_range(lower, -r);
    }

    /// `RangeTriggerNode::setRange`: remember the old bound, move it,
    /// shuffle from the old position (firing whatever the move
    /// crosses), then close the old-range window.
    fn set_range(&mut self, bound: NodeId, range: f32) {
        let old_x = self.x(bound);
        let old_z = self.z(bound);
        // At rest `oldRange_` already holds the old range, so the
        // reference's opening `oldRange_ = range_` is redundant here.
        *self.bound_slots_mut(bound).expect("the bound body").1 = range;
        self.shuffle_x_then_z(bound, old_x, old_z);
        *self.bound_slots_mut(bound).expect("the bound body").2 = range;
    }

    // ----- The sortedness check ---------------------------------------

    /// `RangeList::isSorted`: both chains in non-decreasing order.
    pub fn is_sorted(&self) -> bool {
        let mut prev = self.head;
        while let Some(next) = self.node(prev).next_x {
            if self.x(prev) > self.x(next) {
                return false;
            }
            prev = next;
        }
        let mut prev = self.head;
        while let Some(next) = self.node(prev).next_z {
            if self.z(prev) > self.z(next) {
                return false;
            }
            prev = next;
        }
        true
    }

    /// The X chain, from head to tail, as the test face of `debugDump`.
    pub fn x_order(&self) -> Vec<NodeId> {
        let mut order = Vec::new();
        let mut cursor = self.node(self.head).next_x;
        while let Some(id) = cursor {
            order.push(id);
            cursor = self.node(id).next_x;
        }
        order
    }

    /// The Z chain, from head to tail.
    pub fn z_order(&self) -> Vec<NodeId> {
        let mut order = Vec::new();
        let mut cursor = self.node(self.head).next_z;
        while let Some(id) = cursor {
            order.push(id);
            cursor = self.node(id).next_z;
        }
        order
    }
}

impl fmt::Debug for RangeList {
    /// The CellApp derives `Debug`; the lists report their live census.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let live = self.nodes.iter().filter(|slot| slot.is_some()).count();
        let triggers = self.triggers.iter().filter(|slot| slot.is_some()).count();
        f.debug_struct("RangeList")
            .field("nodes", &live)
            .field("triggers", &triggers)
            .field("sorted", &self.is_sorted())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cellapp_space::CellApp;
    use foundation::math::Vector3;

    /// One trigger's enter/leave ledger.
    type EventLog = Rc<RefCell<Vec<(u32, bool)>>>;

    fn entity_at(id: u32, x: f32, y: f32, z: f32) -> Rc<RefCell<Entity>> {
        let mut entity = Entity::new(id, true);
        entity.position = [x, y, z];
        Rc::new(RefCell::new(entity))
    }

    /// The witness's AoI trigger flag shape (`witness.cpp:73`): the
    /// bounds want AoI crossings from entity nodes and make none of
    /// their own.
    fn make_aoi_trigger(
        list: &mut RangeList,
        central: NodeId,
        range: f32,
    ) -> (TriggerId, EventLog) {
        let events = Rc::new(RefCell::new(Vec::new()));
        let target = TriggerTarget::Recorder(Rc::clone(&events));
        let trigger = list.create_trigger(
            central,
            range,
            FLAG_LOWER_AOI_TRIGGER,
            FLAG_UPPER_AOI_TRIGGER,
            FLAG_NO_TRIGGERS,
            FLAG_NO_TRIGGERS,
            target,
        );
        list.trigger_insert(trigger);
        (trigger, events)
    }

    #[test]
    fn an_insert_inside_the_radius_fires_exactly_one_enter() {
        let mut list = RangeList::new();
        let central = entity_at(1, 0.0, 0.0, 0.0);
        let central_node = list.add_entity_node(Rc::clone(&central));
        let (trigger, events) = make_aoi_trigger(&mut list, central_node, 5.0);

        // A watchee landing inside the area: the X phase is gated out by
        // the insert's `-MAX` old z, the Z phase's `isInX` gate admits
        // it — exactly one enter, never two.
        let watchee = entity_at(2, 2.0, 0.0, 2.0);
        list.add_entity_node(Rc::clone(&watchee));
        assert_eq!(*events.borrow(), vec![(2, true)]);

        assert!(list.is_sorted());
        assert!(list.trigger_contains(trigger, list.x_order()[1]));
    }

    #[test]
    fn an_insert_outside_the_radius_fires_nothing() {
        let mut list = RangeList::new();
        let central = entity_at(1, 0.0, 0.0, 0.0);
        let central_node = list.add_entity_node(Rc::clone(&central));
        let (_trigger, events) = make_aoi_trigger(&mut list, central_node, 5.0);

        let far = entity_at(2, 50.0, 0.0, 50.0);
        list.add_entity_node(Rc::clone(&far));
        assert!(events.borrow().is_empty());
        assert!(list.is_sorted());
    }

    #[test]
    fn the_boundary_excludes_the_lower_and_includes_the_upper() {
        let mut list = RangeList::new();
        let central = entity_at(1, 0.0, 0.0, 0.0);
        let central_node = list.add_entity_node(Rc::clone(&central));
        let (trigger, events) = make_aoi_trigger(&mut list, central_node, 5.0);

        // The reference's own predicate: lower bound exclusive, upper
        // inclusive.
        assert!(!list.is_in_x_range(trigger, -5.0, 5.0));
        assert!(list.is_in_x_range(trigger, 5.0, 5.0));
        assert!(!list.is_in_x_range(trigger, 5.001, 5.0));

        // x = -r sits exactly on the excluded lower bound: no event.
        let below = entity_at(2, -5.0, 0.0, 0.0);
        list.add_entity_node(Rc::clone(&below));
        assert!(events.borrow().is_empty());

        // x = +r sits exactly on the inclusive upper bound: one enter.
        let at = entity_at(3, 5.0, 0.0, 0.0);
        list.add_entity_node(Rc::clone(&at));
        assert_eq!(*events.borrow(), vec![(3, true)]);

        // The order tie-break puts the co-located entity inside the
        // area (`contains` uses `isEqual` plus the orders); the entity
        // on the excluded bound stays out. Chain: the -5 entity sorts
        // before its co-located bound (orders 100 < 190).
        assert!(list.trigger_contains(trigger, list.x_order()[3]));
        assert!(!list.trigger_contains(trigger, list.x_order()[0]));
    }

    #[test]
    fn a_move_across_the_x_boundary_fires_the_enter_then_the_leave() {
        let mut list = RangeList::new();
        let central = entity_at(1, 0.0, 0.0, 0.0);
        let central_node = list.add_entity_node(Rc::clone(&central));
        let (_trigger, events) = make_aoi_trigger(&mut list, central_node, 5.0);

        let watchee = entity_at(2, 10.0, 0.0, 0.0);
        let watchee_node = list.add_entity_node(Rc::clone(&watchee));
        assert!(events.borrow().is_empty());

        // 10 -> 4 crosses the upper bound (+5) inwards.
        watchee.borrow_mut().position[0] = 4.0;
        list.shuffle_entity(watchee_node, &[], 10.0, 0.0);
        assert_eq!(*events.borrow(), vec![(2, true)]);

        // 4 -> 6 crosses it outwards.
        watchee.borrow_mut().position[0] = 6.0;
        list.shuffle_entity(watchee_node, &[], 4.0, 0.0);
        assert_eq!(*events.borrow(), vec![(2, true), (2, false)]);
        assert!(list.is_sorted());
    }

    #[test]
    fn a_move_across_the_z_boundary_fires_the_enter_then_the_leave() {
        let mut list = RangeList::new();
        let central = entity_at(1, 0.0, 0.0, 0.0);
        let central_node = list.add_entity_node(Rc::clone(&central));
        let (_trigger, events) = make_aoi_trigger(&mut list, central_node, 5.0);

        let watchee = entity_at(2, 0.0, 0.0, 10.0);
        let watchee_node = list.add_entity_node(Rc::clone(&watchee));
        assert!(events.borrow().is_empty());

        // 10 -> -4 crosses both Z bounds; the upper crossing's wasInZ
        // gate rejects the outside-in walk, the lower fires the enter.
        watchee.borrow_mut().position[2] = -4.0;
        list.shuffle_entity(watchee_node, &[], 0.0, 10.0);
        assert_eq!(*events.borrow(), vec![(2, true)]);

        // -4 -> 6 crosses the lower bound outwards.
        watchee.borrow_mut().position[2] = 6.0;
        list.shuffle_entity(watchee_node, &[], 0.0, -4.0);
        assert_eq!(*events.borrow(), vec![(2, true), (2, false)]);
        assert!(list.is_sorted());
    }

    #[test]
    fn growing_the_range_catches_and_shrinking_releases() {
        let mut list = RangeList::new();
        let central = entity_at(1, 0.0, 0.0, 0.0);
        let central_node = list.add_entity_node(Rc::clone(&central));
        let (trigger, events) = make_aoi_trigger(&mut list, central_node, 5.0);

        let watchee = entity_at(2, 7.0, 0.0, 0.0);
        list.add_entity_node(Rc::clone(&watchee));
        assert!(events.borrow().is_empty());

        // 5 -> 8: the widening upper bound sweeps over the watchee.
        list.trigger_set_range(trigger, 8.0);
        assert_eq!(*events.borrow(), vec![(2, true)]);
        assert_eq!(list.trigger_range(trigger), 8.0);

        // 8 -> 4: the narrowing upper bound sweeps it back out.
        list.trigger_set_range(trigger, 4.0);
        assert_eq!(*events.borrow(), vec![(2, true), (2, false)]);
        assert_eq!(list.trigger_range(trigger), 4.0);

        // The 1mm floor.
        list.trigger_set_range(trigger, 0.0);
        assert_eq!(list.trigger_range(trigger), 0.001);
        assert!(list.is_sorted());
    }

    #[test]
    fn removing_the_trigger_contracts_to_a_point_and_fires_the_leave() {
        let mut list = RangeList::new();
        let central = entity_at(1, 0.0, 0.0, 0.0);
        let central_node = list.add_entity_node(Rc::clone(&central));
        let (trigger, events) = make_aoi_trigger(&mut list, central_node, 5.0);

        let watchee = entity_at(2, 2.0, 0.0, 0.0);
        list.add_entity_node(Rc::clone(&watchee));
        assert_eq!(*events.borrow(), vec![(2, true)]);

        list.trigger_remove(trigger);
        assert_eq!(*events.borrow(), vec![(2, true), (2, false)]);
        assert!(list.trigger(trigger).is_none());
        assert!(list.is_sorted());
    }

    #[test]
    fn removing_the_entity_sweeps_it_out_through_the_maximum_z() {
        let mut list = RangeList::new();
        let central = entity_at(1, 0.0, 0.0, 0.0);
        let central_node = list.add_entity_node(Rc::clone(&central));
        let (_trigger, events) = make_aoi_trigger(&mut list, central_node, 5.0);

        let inside = entity_at(2, 2.0, 0.0, 2.0);
        let inside_node = list.add_entity_node(Rc::clone(&inside));
        let outside = entity_at(3, 2.0, 0.0, 20.0);
        let outside_node = list.add_entity_node(Rc::clone(&outside));
        assert_eq!(*events.borrow(), vec![(2, true)]);

        // The `z = f32::MAX` walk fires the leave for the watchee
        // inside the area and nothing for the one outside it.
        list.entity_remove(inside_node);
        assert_eq!(*events.borrow(), vec![(2, true), (2, false)]);
        assert_eq!(inside.borrow().position[2], f32::MAX);

        // The departed nodes are off both chains; the trigger's own
        // bounds stay (chain: lower, central, upper, tail) and the
        // lists stay sorted.
        list.entity_remove(outside_node);
        assert!(list.is_sorted());
        assert_eq!(list.x_order().len(), 4);
        assert_eq!(list.z_order().len(), 4);
    }

    #[test]
    fn entity_add_trigger_keeps_the_largest_range_first() {
        let mut list = RangeList::new();
        let central = entity_at(1, 0.0, 0.0, 0.0);
        let central_node = list.add_entity_node(Rc::clone(&central));
        let (inner, _) = make_aoi_trigger(&mut list, central_node, 3.0);
        let (outer, _) = make_aoi_trigger(&mut list, central_node, 8.0);
        let (middle, _) = make_aoi_trigger(&mut list, central_node, 5.0);

        list.entity_add_trigger(&central, inner);
        list.entity_add_trigger(&central, outer);
        list.entity_add_trigger(&central, middle);
        assert_eq!(central.borrow().range_triggers, vec![outer, middle, inner]);

        list.entity_del_trigger(&central, middle);
        assert_eq!(central.borrow().range_triggers, vec![outer, inner]);
    }

    #[test]
    fn the_mover_expands_its_trigger_before_the_move_and_contracts_after() {
        let mut list = RangeList::new();
        let parked = entity_at(1, 0.0, 0.0, 0.0);
        list.add_entity_node(Rc::clone(&parked));

        let mover = entity_at(2, 20.0, 0.0, 0.0);
        let mover_node = list.add_entity_node(Rc::clone(&mover));

        // The mover carries the trigger; parking at (2,0,0) puts the
        // parked entity inside it.
        let (trigger, events) = make_aoi_trigger(&mut list, mover_node, 5.0);
        list.entity_add_trigger(&mover, trigger);
        assert!(events.borrow().is_empty());

        // The expand pass moves the widening lower bound first: it
        // sweeps over the parked entity while the mover itself has not
        // moved yet, so the enter arrives once and the contract (which
        // narrows from the other side, past the now-covered entity)
        // releases nothing.
        mover.borrow_mut().position[0] = 2.0;
        list.shuffle_entity(mover_node, &[trigger], 20.0, 0.0);
        assert_eq!(*events.borrow(), vec![(1, true)]);

        // And the way back fires the matching leave through the
        // contracting upper bound.
        mover.borrow_mut().position[0] = 20.0;
        list.shuffle_entity(mover_node, &[trigger], 2.0, 0.0);
        assert_eq!(*events.borrow(), vec![(1, true), (1, false)]);
        assert!(list.is_sorted());
    }

    #[test]
    fn two_radii_track_the_same_watchee_independently() {
        let mut list = RangeList::new();
        let central = entity_at(1, 0.0, 0.0, 0.0);
        let central_node = list.add_entity_node(Rc::clone(&central));
        let (_outer, outer_events) = make_aoi_trigger(&mut list, central_node, 8.0);
        let (_inner, inner_events) = make_aoi_trigger(&mut list, central_node, 3.0);

        let watchee = entity_at(2, 20.0, 0.0, 0.0);
        let watchee_node = list.add_entity_node(Rc::clone(&watchee));
        assert!(outer_events.borrow().is_empty());
        assert!(inner_events.borrow().is_empty());

        // Landing at x=6: inside the outer area, outside the inner.
        watchee.borrow_mut().position[0] = 6.0;
        list.shuffle_entity(watchee_node, &[], 20.0, 0.0);
        assert_eq!(*outer_events.borrow(), vec![(2, true)]);
        assert!(inner_events.borrow().is_empty());

        // Crossing the inner boundary out at x=2: only the inner
        // trigger releases.
        watchee.borrow_mut().position[0] = 2.0;
        list.shuffle_entity(watchee_node, &[], 6.0, 0.0);
        assert_eq!(*inner_events.borrow(), vec![(2, true)]);

        // And out past the outer boundary entirely.
        watchee.borrow_mut().position[0] = 9.0;
        list.shuffle_entity(watchee_node, &[], 2.0, 0.0);
        assert_eq!(*inner_events.borrow(), vec![(2, true), (2, false)]);
        assert_eq!(*outer_events.borrow(), vec![(2, true), (2, false)]);
        assert!(list.is_sorted());
    }

    #[test]
    fn the_aoi_flag_switch_stops_and_restores_the_crossings() {
        let mut list = RangeList::new();
        let central = entity_at(1, 0.0, 0.0, 0.0);
        let central_node = list.add_entity_node(Rc::clone(&central));
        let (_trigger, events) = make_aoi_trigger(&mut list, central_node, 5.0);

        let watchee = entity_at(2, 10.0, 0.0, 0.0);
        let watchee_node = list.add_entity_node(Rc::clone(&watchee));

        // An entity whose AoI flags are down (it carries an appeal
        // trigger instead) makes no AoI crossings: the boundary walks
        // in and back out are both silent.
        list.set_entity_aoi_trigger(watchee_node, false);
        watchee.borrow_mut().position[0] = 2.0;
        list.shuffle_entity(watchee_node, &[], 10.0, 0.0);
        watchee.borrow_mut().position[0] = 10.0;
        list.shuffle_entity(watchee_node, &[], 2.0, 0.0);
        assert!(events.borrow().is_empty());

        // The flags come back up while the entity is outside, and the
        // next walk fires normally — in and out.
        list.set_entity_aoi_trigger(watchee_node, true);
        watchee.borrow_mut().position[0] = 2.0;
        list.shuffle_entity(watchee_node, &[], 10.0, 0.0);
        assert_eq!(*events.borrow(), vec![(2, true)]);
        watchee.borrow_mut().position[0] = 10.0;
        list.shuffle_entity(watchee_node, &[], 2.0, 0.0);
        assert_eq!(*events.borrow(), vec![(2, true), (2, false)]);
        assert!(list.is_sorted());
    }

    #[test]
    fn an_aoi_toggle_on_a_bound_node_is_inert() {
        let mut list = RangeList::new();
        let central = entity_at(1, 0.0, 0.0, 0.0);
        let central_node = list.add_entity_node(Rc::clone(&central));
        let (trigger, _) = make_aoi_trigger(&mut list, central_node, 5.0);
        let bound = list.trigger(trigger).expect("live trigger").upper;
        let makes_before = list.node(bound).makes;

        list.set_entity_aoi_trigger(bound, false);
        assert_eq!(list.node(bound).makes, makes_before);
    }

    #[test]
    fn the_position_funnel_drives_the_shuffle() {
        let mut app = CellApp::new();

        let central = entity_at(1, 0.0, 0.0, 0.0);
        app.entity_population.insert(1, Rc::clone(&central));
        let central_node = app
            .range_lists
            .entry(0)
            .or_default()
            .add_entity_node(Rc::clone(&central));
        central.borrow_mut().range_node = Some(central_node);

        let watchee = entity_at(2, 20.0, 0.0, 0.0);
        app.entity_population.insert(2, Rc::clone(&watchee));
        let watchee_node = app
            .range_lists
            .entry(0)
            .or_default()
            .add_entity_node(Rc::clone(&watchee));
        watchee.borrow_mut().range_node = Some(watchee_node);

        let central_node = central.borrow().range_node.expect("wired");
        let (trigger, events) = make_aoi_trigger(
            app.range_lists.get_mut(&0).expect("space 0"),
            central_node,
            5.0,
        );
        app.range_lists
            .get_mut(&0)
            .expect("space 0")
            .entity_add_trigger(&central, trigger);

        // The script position setter runs the internals: the shuffle
        // fires the enter, the physics correction rides the same call.
        app.set_global_position(&watchee, Vector3::new(2.0, 0.0, 0.0));
        assert_eq!(*events.borrow(), vec![(2, true)]);
        assert_eq!(app.physics_corrections, vec![2]);

        app.set_global_position(&watchee, Vector3::new(6.0, 0.0, 0.0));
        assert_eq!(*events.borrow(), vec![(2, true), (2, false)]);
        assert_eq!(app.physics_corrections, vec![2, 2]);
        assert!(app.range_lists.get(&0).expect("space 0").is_sorted());

        // An entity that never went through the real-entity builder
        // carries no range node: the range face skips it and the ride
        // continues.
        let bystander = entity_at(3, 50.0, 0.0, 0.0);
        app.entity_population.insert(3, Rc::clone(&bystander));
        app.set_global_position(&bystander, Vector3::new(60.0, 0.0, 0.0));
        assert_eq!(bystander.borrow().position, [60.0, 0.0, 0.0]);

        // A wired node in a space whose lists are held elsewhere
        // (no range_lists entry) drives the listless-space skip.
        let stranger = entity_at(4, 0.0, 0.0, 0.0);
        app.entity_population.insert(4, Rc::clone(&stranger));
        stranger.borrow_mut().range_node = Some(0);
        stranger.borrow_mut().space_id = 9;
        app.set_global_position(&stranger, Vector3::new(1.0, 0.0, 0.0));
        assert_eq!(stranger.borrow().position, [1.0, 0.0, 0.0]);
    }

    /// `RangeListAppealTrigger`'s inverted flag shape: the bounds want
    /// nothing and make AoI crossings of the opposite side — the big
    /// entity's footprint for other witnesses.
    fn make_appeal_trigger(
        list: &mut RangeList,
        central: NodeId,
        range: f32,
    ) -> (TriggerId, EventLog) {
        let events = Rc::new(RefCell::new(Vec::new()));
        let target = TriggerTarget::Recorder(Rc::clone(&events));
        let trigger = list.create_trigger(
            central,
            range,
            FLAG_NO_TRIGGERS,
            FLAG_NO_TRIGGERS,
            FLAG_UPPER_AOI_TRIGGER,
            FLAG_LOWER_AOI_TRIGGER,
            target,
        );
        list.trigger_insert(trigger);
        (trigger, events)
    }

    #[test]
    fn an_appeal_bound_crossing_fires_the_enter_then_the_leave_on_x() {
        let mut list = RangeList::new();
        let observer = entity_at(1, 0.0, 0.0, 0.0);
        let observer_node = list.add_entity_node(Rc::clone(&observer));
        let (_, events) = make_aoi_trigger(&mut list, observer_node, 5.0);

        // The big entity carries an appeal trigger instead of its own
        // AoI crossings (`isAoITrigger(false)`).
        let big = entity_at(2, 20.0, 0.0, 0.0);
        let big_node = list.add_entity_node(Rc::clone(&big));
        list.set_entity_aoi_trigger(big_node, false);
        let (appeal, appeal_events) = make_appeal_trigger(&mut list, big_node, 10.0);
        assert!(events.borrow().is_empty());
        assert!(appeal_events.borrow().is_empty());

        // Walking in: the appeal's lower bound sweeps across the
        // witness's upper bound, and the witness reports the big entity
        // behind the appeal (`crossedXEntityRange`).
        big.borrow_mut().position[0] = 2.0;
        list.shuffle_entity(big_node, &[appeal], 20.0, 0.0);
        assert_eq!(*events.borrow(), vec![(2, true)]);
        assert!(appeal_events.borrow().is_empty());

        // And back out: the trailing lower bound contracts across the
        // same witness bound and the leave fires through the old span.
        big.borrow_mut().position[0] = 20.0;
        list.shuffle_entity(big_node, &[appeal], 2.0, 0.0);
        assert_eq!(*events.borrow(), vec![(2, true), (2, false)]);
        assert!(appeal_events.borrow().is_empty());
        assert!(list.is_sorted());
    }

    #[test]
    fn an_appeal_bound_crossing_fires_the_enter_then_the_leave_on_z() {
        let mut list = RangeList::new();
        let observer = entity_at(1, 0.0, 0.0, 0.0);
        let observer_node = list.add_entity_node(Rc::clone(&observer));
        let (_, events) = make_aoi_trigger(&mut list, observer_node, 5.0);

        let big = entity_at(2, 0.0, 0.0, 20.0);
        let big_node = list.add_entity_node(Rc::clone(&big));
        list.set_entity_aoi_trigger(big_node, false);
        let (appeal, appeal_events) = make_appeal_trigger(&mut list, big_node, 10.0);
        assert!(events.borrow().is_empty());
        assert!(appeal_events.borrow().is_empty());

        big.borrow_mut().position[2] = 2.0;
        list.shuffle_entity(big_node, &[appeal], 0.0, 20.0);
        assert_eq!(*events.borrow(), vec![(2, true)]);
        assert!(appeal_events.borrow().is_empty());

        big.borrow_mut().position[2] = 20.0;
        list.shuffle_entity(big_node, &[appeal], 0.0, 2.0);
        assert_eq!(*events.borrow(), vec![(2, true), (2, false)]);
        assert!(appeal_events.borrow().is_empty());
        assert!(list.is_sorted());
    }

    #[test]
    fn an_appeal_x_crossing_gated_out_by_the_old_z_span_stays_silent() {
        let mut list = RangeList::new();
        let observer = entity_at(1, 0.0, 0.0, 0.0);
        let observer_node = list.add_entity_node(Rc::clone(&observer));
        let (_, events) = make_aoi_trigger(&mut list, observer_node, 5.0);

        // Far above in Z: the X crossing dispatches, but the appeal's
        // old Z span [90, 110] never met the witness's ±5.
        let big = entity_at(2, 20.0, 0.0, 100.0);
        let big_node = list.add_entity_node(Rc::clone(&big));
        list.set_entity_aoi_trigger(big_node, false);
        let (appeal, _) = make_appeal_trigger(&mut list, big_node, 10.0);

        big.borrow_mut().position[0] = 2.0;
        list.shuffle_entity(big_node, &[appeal], 20.0, 100.0);
        assert!(events.borrow().is_empty());

        big.borrow_mut().position[0] = 20.0;
        list.shuffle_entity(big_node, &[appeal], 2.0, 100.0);
        assert!(events.borrow().is_empty());
        assert!(list.is_sorted());
    }

    #[test]
    fn an_appeal_z_crossing_gated_out_by_the_new_x_span_stays_silent() {
        let mut list = RangeList::new();
        let observer = entity_at(1, 0.0, 0.0, 0.0);
        let observer_node = list.add_entity_node(Rc::clone(&observer));
        let (_, events) = make_aoi_trigger(&mut list, observer_node, 5.0);

        // Far out in X: the Z crossing dispatches, but the appeal's X
        // span [90, 110] never meets the witness's ±5.
        let big = entity_at(2, 100.0, 0.0, 20.0);
        let big_node = list.add_entity_node(Rc::clone(&big));
        list.set_entity_aoi_trigger(big_node, false);
        let (appeal, _) = make_appeal_trigger(&mut list, big_node, 10.0);

        big.borrow_mut().position[2] = 2.0;
        list.shuffle_entity(big_node, &[appeal], 100.0, 20.0);
        assert!(events.borrow().is_empty());

        big.borrow_mut().position[2] = 20.0;
        list.shuffle_entity(big_node, &[appeal], 100.0, 2.0);
        assert!(events.borrow().is_empty());
        assert!(list.is_sorted());
    }

    #[test]
    fn inserting_a_trigger_walks_past_co_located_nodes_and_fires_one_enter() {
        let mut list = RangeList::new();
        let first = entity_at(1, 0.0, 0.0, 0.0);
        let first_node = list.add_entity_node(Rc::clone(&first));
        let second = entity_at(2, 0.0, 0.0, 0.0);
        let second_node = list.add_entity_node(Rc::clone(&second));

        // The reference's `insert()` cursor walk places both bounds past
        // the co-located nodes. The X expansion then crosses the node
        // with the old-range window still closed (the `wasInZRange` gate
        // rejects), and the Z expansion delivers exactly one enter.
        let (trigger, events) = make_aoi_trigger(&mut list, first_node, 5.0);
        assert_eq!(*events.borrow(), vec![(2, true)]);
        assert!(list.trigger_contains(trigger, second_node));
        assert!(list.trigger_contains_in_z(trigger, second_node));
        assert!(list.is_sorted());
    }

    #[test]
    fn trigger_shuffle_x_then_z_moves_both_bounds_with_the_central_entity() {
        let mut list = RangeList::new();
        let central = entity_at(1, 0.0, 0.0, 0.0);
        let central_node = list.add_entity_node(Rc::clone(&central));
        let (trigger, events) = make_aoi_trigger(&mut list, central_node, 5.0);

        let watchee = entity_at(2, 8.0, 0.0, 0.0);
        let watchee_node = list.add_entity_node(Rc::clone(&watchee));
        assert!(events.borrow().is_empty());
        let _ = watchee_node;

        // The trigger's own shuffle face: both bounds move with the
        // central entity, the trailing upper bound sweeping the watchee
        // in, and the stored old central position catches up after.
        central.borrow_mut().position[0] = 4.0;
        list.trigger_shuffle_x_then_z(trigger, 0.0, 0.0);
        assert_eq!(*events.borrow(), vec![(2, true)]);
        assert_eq!(
            list.trigger(trigger).expect("live trigger").old_entity_x,
            4.0
        );

        central.borrow_mut().position[0] = 0.0;
        list.trigger_shuffle_x_then_z(trigger, 4.0, 0.0);
        assert_eq!(*events.borrow(), vec![(2, true), (2, false)]);
        assert_eq!(
            list.trigger(trigger).expect("live trigger").old_entity_x,
            0.0
        );
        assert!(list.is_sorted());
    }

    #[test]
    fn removed_slots_are_recycled_by_the_next_creations() {
        let mut list = RangeList::new();
        let a = list.add_entity_node(entity_at(1, 0.0, 0.0, 0.0));
        let (trigger, events) = make_aoi_trigger(&mut list, a, 5.0);
        let b = list.add_entity_node(entity_at(2, 2.0, 0.0, 0.0));
        assert_eq!(*events.borrow(), vec![(2, true)]);

        // Removing both frees the node and trigger slots...
        list.trigger_remove(trigger);
        list.entity_remove(b);
        list.entity_remove(a);
        assert_eq!(*events.borrow(), vec![(2, true), (2, false)]);

        // ...and the next creations recycle them: the trigger id comes
        // back off the free list, and a fresh pair fires the enter.
        let c = list.add_entity_node(entity_at(3, 0.0, 0.0, 0.0));
        let (again, events) = make_aoi_trigger(&mut list, c, 5.0);
        assert_eq!(again, trigger, "the trigger id is recycled");
        let _d = list.add_entity_node(entity_at(4, 2.0, 0.0, 0.0));
        assert_eq!(*events.borrow(), vec![(4, true)]);
        assert!(list.is_sorted());
    }

    #[test]
    fn a_stale_chain_reports_unsorted() {
        let mut list = RangeList::new();
        let left = entity_at(1, 0.0, 0.0, 0.0);
        let left_node = list.add_entity_node(Rc::clone(&left));
        let right = entity_at(2, 5.0, 0.0, 0.0);
        let right_node = list.add_entity_node(Rc::clone(&right));
        assert!(list.is_sorted());

        // A raw position write skips the shuffle: the X chain goes
        // stale and the census reports it.
        left.borrow_mut().position[0] = 10.0;
        assert!(!list.is_sorted());

        // Restore X, stale the Z chain the same way.
        left.borrow_mut().position[0] = 0.0;
        assert!(list.is_sorted());
        left.borrow_mut().position[2] = 10.0;
        assert!(!list.is_sorted());

        left.borrow_mut().position[2] = 0.0;
        assert!(list.is_sorted());
        let _ = (left_node, right_node);
    }

    #[test]
    fn the_debug_face_reports_the_live_census() {
        let mut list = RangeList::new();
        let central = entity_at(1, 0.0, 0.0, 0.0);
        let central_node = list.add_entity_node(Rc::clone(&central));
        let (trigger, _) = make_aoi_trigger(&mut list, central_node, 5.0);
        assert!(list.trigger(trigger).is_some());

        // Terminators, the entity node, and the two bounds; one trigger.
        let text = format!("{:?}", list);
        assert!(text.contains("nodes: 5"), "{text}");
        assert!(text.contains("triggers: 1"), "{text}");
        assert!(text.contains("sorted: true"), "{text}");
    }

    #[test]
    fn the_bound_face_reads_none_outside_a_bound_node() {
        let mut list = RangeList::new();
        let central = entity_at(1, 0.0, 0.0, 0.0);
        let central_node = list.add_entity_node(Rc::clone(&central));
        let (trigger, _) = make_aoi_trigger(&mut list, central_node, 5.0);
        let (upper, lower) = {
            let data = list.trigger(trigger).expect("live trigger");
            (data.upper, data.lower)
        };

        // The bound triple reads off a bound...
        assert_eq!(list.as_bound(upper).map(|(_, r, _)| r), Some(5.0));
        assert_eq!(list.as_bound(lower).map(|(_, r, _)| r), Some(-5.0));
        // ...and `None` off the entity and terminator bodies, the same
        // face the mut slots mirror.
        assert!(list.as_bound(central_node).is_none());
        assert!(list.as_bound(list.head).is_none());
        assert!(list.entity_of(upper).is_none());
        assert!(list.bound_slots_mut(central_node).is_none());
        assert_eq!(
            list.bound_slots_mut(upper).map(|(_, range, _)| *range),
            Some(5.0)
        );
    }

    #[test]
    fn a_fast_dash_across_the_whole_area_fires_nothing() {
        let mut list = RangeList::new();
        let observer = entity_at(1, 0.0, 0.0, 0.0);
        let observer_node = list.add_entity_node(Rc::clone(&observer));
        let (_, events) = make_aoi_trigger(&mut list, observer_node, 5.0);

        let runner = entity_at(2, -10.0, 0.0, 0.0);
        let runner_node = list.add_entity_node(Rc::clone(&runner));
        assert!(events.borrow().is_empty());

        // One hop from far left to just past the upper bound: the lower
        // crossing's enter is rejected (the new x is already out), and
        // the upper crossing's leave is rejected (the old x was never
        // in) — the mover was never seen.
        runner.borrow_mut().position[0] = 6.0;
        list.shuffle_entity(runner_node, &[], -10.0, 0.0);
        assert!(events.borrow().is_empty());
        assert!(list.is_sorted());
    }

    #[test]
    fn an_appeal_z_jump_across_the_area_stays_silent() {
        let mut list = RangeList::new();
        let observer = entity_at(1, 0.0, 0.0, 0.0);
        let observer_node = list.add_entity_node(Rc::clone(&observer));
        let (_, events) = make_aoi_trigger(&mut list, observer_node, 5.0);

        let big = entity_at(2, 0.0, 0.0, 20.0);
        let big_node = list.add_entity_node(Rc::clone(&big));
        list.set_entity_aoi_trigger(big_node, false);
        let (appeal, _) = make_appeal_trigger(&mut list, big_node, 10.0);

        // One hop from far above to far below: the descending lower
        // bound's enter is rejected (the new Z span is already out), and
        // the trailing upper bound's leave is rejected (the old Z span
        // was never in).
        big.borrow_mut().position[2] = -20.0;
        list.shuffle_entity(big_node, &[appeal], 0.0, 20.0);
        assert!(events.borrow().is_empty());
        assert!(list.is_sorted());
    }

    #[test]
    fn an_appeal_approach_that_never_arrives_stays_silent() {
        let mut list = RangeList::new();
        let observer = entity_at(1, 0.0, 0.0, 0.0);
        let observer_node = list.add_entity_node(Rc::clone(&observer));
        let (_, events) = make_aoi_trigger(&mut list, observer_node, 5.0);

        let big = entity_at(2, 20.0, 0.0, 0.0);
        let big_node = list.add_entity_node(Rc::clone(&big));
        list.set_entity_aoi_trigger(big_node, false);
        let (appeal, _) = make_appeal_trigger(&mut list, big_node, 10.0);

        // The x walk-in passes the witness bound, but the entity's new
        // home is far above: the old-Z gate opens and the new-Z span
        // closes it again. The trailing Z contract rejects the leave
        // through the old-X span.
        big.borrow_mut().position = [2.0, 0.0, 100.0];
        list.shuffle_entity(big_node, &[appeal], 20.0, 0.0);
        assert!(events.borrow().is_empty());
        assert!(list.is_sorted());
    }

    #[test]
    fn an_appeal_dash_across_the_whole_area_stays_silent() {
        let mut list = RangeList::new();
        let observer = entity_at(1, 0.0, 0.0, 0.0);
        let observer_node = list.add_entity_node(Rc::clone(&observer));
        let (_, events) = make_aoi_trigger(&mut list, observer_node, 5.0);

        let big = entity_at(2, -20.0, 0.0, 0.0);
        let big_node = list.add_entity_node(Rc::clone(&big));
        list.set_entity_aoi_trigger(big_node, false);
        let (appeal, _) = make_appeal_trigger(&mut list, big_node, 10.0);

        // One hop from far left to far right: the expanding upper bound
        // crosses the witness's lower bound with a new-X span already
        // past it, and the contracting lower bound crosses the witness's
        // upper bound with an old-X span that was never inside — the
        // mover was never seen.
        big.borrow_mut().position[0] = 40.0;
        list.shuffle_entity(big_node, &[appeal], -20.0, 0.0);
        assert!(events.borrow().is_empty());
        assert!(list.is_sorted());
    }

    #[test]
    fn deleting_an_unregistered_trigger_is_inert() {
        let mut list = RangeList::new();
        let central = entity_at(1, 0.0, 0.0, 0.0);
        let central_node = list.add_entity_node(Rc::clone(&central));
        let (trigger, _) = make_aoi_trigger(&mut list, central_node, 5.0);
        list.entity_add_trigger(&central, trigger);

        // A trigger the entity never carries comes off silently.
        list.entity_del_trigger(&central, 999);
        assert_eq!(central.borrow().range_triggers, vec![trigger]);
    }
}
