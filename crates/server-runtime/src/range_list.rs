//! BW-08-03-01: the cell range list and its range triggers.
//!
//! Port of `cell_range_list.*` (`RangeList`), `range_list_node.*`
//! (`RangeListNode`/`RangeListTerminator`), `range_trigger.*`
//! (`RangeTriggerNode`/`RangeTrigger`), `range_list_appeal_trigger.*`
//! (`RangeListAppealTrigger`) and `entity_range_list_node.*`
//! (`EntityRangeListNode`).
//!
//! The reference keeps a pair of intrusive doubly-linked lists (X and Z axes)
//! threaded through every node; entities, trigger bounds and the head/tail
//! terminators all live in the same lists. Nodes carry `wantsFlags` /
//! `makesFlags`; a shuffle only announces a crossing to a node that `wants`
//! what the other `makes`. Enter/leave decisions are candidate events from
//! list crossings, validated against the trigger's geometry gates
//! (`isIn*Range` for enters, `wasIn*Range` for leaves), so the events
//! reproduce the reference exactly, including its half-open `(lower, upper]`
//! boundary.

/// Index into [`RangeListWorld::nodes`].
pub type NodeRef = usize;
/// Index into the world's trigger table.
pub type TriggerRef = usize;

/// `RangeListNode::RangeListFlags`.
pub const FLAG_NO_TRIGGERS: u16 = 0;
pub const FLAG_ENTITY_TRIGGER: u16 = 0x01;
pub const FLAG_LOWER_AOI_TRIGGER: u16 = 0x02;
pub const FLAG_UPPER_AOI_TRIGGER: u16 = 0x04;
pub const FLAG_IS_ENTITY: u16 = 0x10;
pub const FLAG_IS_LOWER_BOUND: u16 = 0x20;

/// `RangeListOrder`.
pub const RANGE_LIST_ORDER_HEAD: u16 = 0;
pub const RANGE_LIST_ORDER_ENTITY: u16 = 100;
pub const RANGE_LIST_ORDER_LOWER_BOUND: u16 = 190;
pub const RANGE_LIST_ORDER_UPPER_BOUND: u16 = 200;
pub const RANGE_LIST_ORDER_TAIL: u16 = 0xffff;

/// A trigger enter/leave announced by the range list machinery. `trigger` is
/// the observer's trigger; `entity_id` is the entity being seen (or lost).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RangeListEvent {
    TriggerEnter { trigger: TriggerRef, entity_id: u32 },
    TriggerLeave { trigger: TriggerRef, entity_id: u32 },
}

/// `RangeTriggerNode`: offset from the trigger's central node by `range`.
#[derive(Debug, Clone, Copy)]
struct BoundData {
    trigger: TriggerRef,
    range: f32,
    old_range: f32,
}

#[derive(Debug, Clone, Copy)]
enum NodeKind {
    /// `RangeListTerminator`: always at +/- FLT_MAX.
    Terminator {
        head: bool,
    },
    /// `EntityRangeListNode`: position is the entity's position. Whether the
    /// node is an AOI trigger is the state of its AOI makes flags, exactly
    /// as in the reference.
    Entity {
        entity_id: u32,
        x: f32,
        z: f32,
    },
    Bound(BoundData),
}

#[derive(Debug, Clone, Copy)]
struct RangeNode {
    prev_x: Option<NodeRef>,
    next_x: Option<NodeRef>,
    prev_z: Option<NodeRef>,
    next_z: Option<NodeRef>,
    wants_flags: u16,
    makes_flags: u16,
    order: u16,
    kind: NodeKind,
}

/// `RangeTrigger`: an upper and a lower bound around a central node, plus the
/// central node's previous location (used by the `wasIn*Range` gates).
struct RangeTriggerData {
    central: NodeRef,
    upper: NodeRef,
    lower: NodeRef,
    old_entity_x: f32,
    old_entity_z: f32,
}

/// The cell's range list world: one `RangeList` (both axes) plus the triggers
/// that move with their entities.
///
/// `Entity::triggers_` (sorted descending by range) is `triggers_of_entity`;
/// `Space::appealRadiusList_` is `appeal_triggers`; enter/leave events are
/// recorded into `events` in reference firing order.
pub struct RangeListWorld {
    nodes: Vec<RangeNode>,
    triggers: Vec<RangeTriggerData>,
    head: NodeRef,
    entity_nodes: std::collections::BTreeMap<u32, NodeRef>,
    triggers_of_entity: std::collections::BTreeMap<u32, Vec<TriggerRef>>,
    appeal_triggers: Vec<TriggerRef>,
    /// The reference's function-local `static bool inShuffle` reentrancy guard.
    in_shuffle: bool,
    /// Enter/leave events in reference firing order.
    pub events: Vec<RangeListEvent>,
}

impl RangeListWorld {
    pub fn new() -> Self {
        let mut nodes = Vec::new();
        // RangeList::RangeList: first_(true), last_(false), with
        // last_.insertBeforeX/Z(&first_) making head <-> tail.
        let head = nodes.len();
        nodes.push(RangeNode {
            prev_x: None,
            next_x: None,
            prev_z: None,
            next_z: None,
            wants_flags: FLAG_NO_TRIGGERS,
            makes_flags: FLAG_NO_TRIGGERS,
            order: RANGE_LIST_ORDER_HEAD,
            kind: NodeKind::Terminator { head: true },
        });
        let tail = nodes.len();
        nodes.push(RangeNode {
            prev_x: None,
            next_x: None,
            prev_z: None,
            next_z: None,
            wants_flags: FLAG_NO_TRIGGERS,
            makes_flags: FLAG_NO_TRIGGERS,
            order: RANGE_LIST_ORDER_TAIL,
            kind: NodeKind::Terminator { head: false },
        });
        let mut world = Self {
            nodes,
            triggers: Vec::new(),
            head,
            entity_nodes: std::collections::BTreeMap::new(),
            triggers_of_entity: std::collections::BTreeMap::new(),
            appeal_triggers: Vec::new(),
            in_shuffle: false,
            events: Vec::new(),
        };
        world.insert_before_x(tail, head);
        world.insert_before_z(tail, head);
        world
    }

    // ------------------------------------------------------------------
    // RangeListNode faces
    // ------------------------------------------------------------------

    fn node_x(&self, node: NodeRef) -> f32 {
        match self.nodes[node].kind {
            NodeKind::Terminator { head } => {
                // order_ ? FLT_MAX : -FLT_MAX
                if head { -f32::MAX } else { f32::MAX }
            }
            NodeKind::Entity { x, .. } => x,
            NodeKind::Bound(BoundData { trigger, range, .. }) => {
                self.node_x(self.triggers[trigger].central) + range
            }
        }
    }

    fn node_z(&self, node: NodeRef) -> f32 {
        match self.nodes[node].kind {
            NodeKind::Terminator { head } => {
                if head {
                    -f32::MAX
                } else {
                    f32::MAX
                }
            }
            NodeKind::Entity { z, .. } => z,
            NodeKind::Bound(BoundData { trigger, range, .. }) => {
                self.node_z(self.triggers[trigger].central) + range
            }
        }
    }

    fn bound_old_x(&self, node: NodeRef) -> f32 {
        let bound = self.bound(node);
        self.triggers[bound.trigger].old_entity_x + bound.old_range
    }

    fn bound_old_z(&self, node: NodeRef) -> f32 {
        let bound = self.bound(node);
        self.triggers[bound.trigger].old_entity_z + bound.old_range
    }

    fn bound(&self, node: NodeRef) -> &BoundData {
        match &self.nodes[node].kind {
            NodeKind::Bound(bound) => bound,
            _ => panic!("expected a range trigger bound node"),
        }
    }

    fn bound_mut(&mut self, node: NodeRef) -> &mut BoundData {
        match &mut self.nodes[node].kind {
            NodeKind::Bound(bound) => bound,
            _ => panic!("expected a range trigger bound node"),
        }
    }

    fn is_entity(&self, node: NodeRef) -> bool {
        self.nodes[node].makes_flags & FLAG_IS_ENTITY != 0
    }

    fn is_lower_bound(&self, node: NodeRef) -> bool {
        self.nodes[node].makes_flags & FLAG_IS_LOWER_BOUND != 0
    }

    fn entity_id_of(&self, node: NodeRef) -> u32 {
        match self.nodes[node].kind {
            NodeKind::Entity { entity_id, .. } => entity_id,
            _ => panic!("EntityRangeListNode::getEntity on a non-entity node"),
        }
    }

    /// `EntityRangeListNode::x/z` readback.
    fn entity_position(&self, node: NodeRef) -> (f32, f32) {
        match self.nodes[node].kind {
            NodeKind::Entity { x, z, .. } => (x, z),
            _ => unreachable!("entity node"),
        }
    }

    fn set_entity_position(&mut self, node: NodeRef, x: f32, z: f32) {
        match &mut self.nodes[node].kind {
            NodeKind::Entity { x: px, z: pz, .. } => {
                *px = x;
                *pz = z;
            }
            _ => unreachable!("entity node"),
        }
    }

    fn wants_crossing_with(&self, node: NodeRef, other: NodeRef) -> bool {
        self.nodes[node].wants_flags & self.nodes[other].makes_flags != 0
    }

    /// `RangeListNode::insertBeforeX`.
    fn insert_before_x(&mut self, node: NodeRef, other: NodeRef) {
        debug_assert!(node != other, "MF_ASSERT( this != pNode )");
        if let Some(prev) = self.nodes[node].prev_x {
            self.nodes[prev].next_x = Some(other);
        }
        self.nodes[other].prev_x = self.nodes[node].prev_x;
        self.nodes[node].prev_x = Some(other);
        self.nodes[other].next_x = Some(node);
    }

    /// `RangeListNode::insertBeforeZ`.
    fn insert_before_z(&mut self, node: NodeRef, other: NodeRef) {
        debug_assert!(node != other, "MF_ASSERT( this != pNode )");
        if let Some(prev) = self.nodes[node].prev_z {
            self.nodes[prev].next_z = Some(other);
        }
        self.nodes[other].prev_z = self.nodes[node].prev_z;
        self.nodes[node].prev_z = Some(other);
        self.nodes[other].next_z = Some(node);
    }

    /// `RangeListNode::removeFromRangeList`.
    fn remove_from_range_list(&mut self, node: NodeRef) {
        self.nodes[node]
            .prev_x
            .inspect(|prev| self.nodes[*prev].next_x = self.nodes[node].next_x);
        self.nodes[node]
            .next_x
            .inspect(|next| self.nodes[*next].prev_x = self.nodes[node].prev_x);
        self.nodes[node]
            .prev_z
            .inspect(|prev| self.nodes[*prev].next_z = self.nodes[node].next_z);
        self.nodes[node]
            .next_z
            .inspect(|next| self.nodes[*next].prev_z = self.nodes[node].prev_z);
        self.nodes[node].prev_x = None;
        self.nodes[node].next_x = None;
        self.nodes[node].prev_z = None;
        self.nodes[node].next_z = None;
    }

    /// `RangeListNode::shuffleXThenZ`.
    fn shuffle_x_then_z(&mut self, node: NodeRef, old_x: f32, old_z: f32) {
        self.shuffle_x(node, old_x, old_z);
        self.shuffle_z(node, old_x, old_z);
    }

    /// `RangeListNode::shuffleX`.
    fn shuffle_x(&mut self, node: NodeRef, old_x: f32, old_z: f32) {
        debug_assert!(!self.in_shuffle, "make sure we are not reentrant");
        self.in_shuffle = true;

        let our_pos = self.node_x(node);

        // Shuffle to the left (negative X)...
        while let Some(prev) = self.nodes[node].prev_x {
            let oth_pos = self.node_x(prev);
            if !(our_pos < oth_pos
                || (our_pos == oth_pos && self.nodes[node].order <= self.nodes[prev].order))
            {
                break;
            }
            if self.wants_crossing_with(node, prev) {
                let (ox, oz) = (self.node_x(prev), self.node_z(prev));
                self.crossed_x(node, prev, true, ox, oz);
            }
            if self.wants_crossing_with(prev, node) {
                self.crossed_x(prev, node, false, old_x, old_z);
            }
            // unlink us
            self.nodes[node]
                .next_x
                .inspect(|next| self.nodes[*next].prev_x = Some(prev));
            self.nodes[prev].next_x = self.nodes[node].next_x;
            // fix our pointers
            self.nodes[node].next_x = Some(prev);
            self.nodes[node].prev_x = self.nodes[prev].prev_x;
            // relink us
            self.nodes[node]
                .prev_x
                .inspect(|pp| self.nodes[*pp].next_x = Some(node));
            self.nodes[prev].prev_x = Some(node);
        }

        // Shuffle to the right (positive X)...
        while let Some(next) = self.nodes[node].next_x {
            let oth_pos = self.node_x(next);
            if !(our_pos > oth_pos
                || (our_pos == oth_pos && self.nodes[node].order >= self.nodes[next].order))
            {
                break;
            }
            if self.wants_crossing_with(node, next) {
                let (ox, oz) = (self.node_x(next), self.node_z(next));
                self.crossed_x(node, next, false, ox, oz);
            }
            if self.wants_crossing_with(next, node) {
                self.crossed_x(next, node, true, old_x, old_z);
            }
            // unlink us
            self.nodes[node]
                .prev_x
                .inspect(|prev| self.nodes[*prev].next_x = Some(next));
            self.nodes[next].prev_x = self.nodes[node].prev_x;
            // fix our pointers
            self.nodes[node].prev_x = Some(next);
            self.nodes[node].next_x = self.nodes[next].next_x;
            // relink us
            self.nodes[next].next_x = Some(node);
            self.nodes[node]
                .next_x
                .inspect(|nn| self.nodes[*nn].prev_x = Some(node));
        }

        self.in_shuffle = false;
    }

    /// `RangeListNode::shuffleZ`.
    fn shuffle_z(&mut self, node: NodeRef, old_x: f32, old_z: f32) {
        debug_assert!(!self.in_shuffle, "make sure we are not reentrant");
        self.in_shuffle = true;

        let our_pos = self.node_z(node);

        while let Some(prev) = self.nodes[node].prev_z {
            let oth_pos = self.node_z(prev);
            if !(our_pos < oth_pos
                || (our_pos == oth_pos && self.nodes[node].order <= self.nodes[prev].order))
            {
                break;
            }
            if self.wants_crossing_with(node, prev) {
                let (ox, oz) = (self.node_x(prev), self.node_z(prev));
                self.crossed_z(node, prev, true, ox, oz);
            }
            if self.wants_crossing_with(prev, node) {
                self.crossed_z(prev, node, false, old_x, old_z);
            }
            self.nodes[node]
                .next_z
                .inspect(|next| self.nodes[*next].prev_z = Some(prev));
            self.nodes[prev].next_z = self.nodes[node].next_z;
            self.nodes[node].next_z = Some(prev);
            self.nodes[node].prev_z = self.nodes[prev].prev_z;
            self.nodes[node]
                .prev_z
                .inspect(|pp| self.nodes[*pp].next_z = Some(node));
            self.nodes[prev].prev_z = Some(node);
        }

        while let Some(next) = self.nodes[node].next_z {
            let oth_pos = self.node_z(next);
            if !(our_pos > oth_pos
                || (our_pos == oth_pos && self.nodes[node].order >= self.nodes[next].order))
            {
                break;
            }
            if self.wants_crossing_with(node, next) {
                let (ox, oz) = (self.node_x(next), self.node_z(next));
                self.crossed_z(node, next, false, ox, oz);
            }
            if self.wants_crossing_with(next, node) {
                self.crossed_z(next, node, true, old_x, old_z);
            }
            self.nodes[node]
                .prev_z
                .inspect(|prev| self.nodes[*prev].next_z = Some(next));
            self.nodes[next].prev_z = self.nodes[node].prev_z;
            self.nodes[node].prev_z = Some(next);
            self.nodes[node].next_z = self.nodes[next].next_z;
            self.nodes[next].next_z = Some(node);
            self.nodes[node]
                .next_z
                .inspect(|nn| self.nodes[*nn].prev_z = Some(node));
        }

        self.in_shuffle = false;
    }

    // ------------------------------------------------------------------
    // RangeTriggerNode::crossedX / crossedZ
    // ------------------------------------------------------------------

    fn crossed_x(
        &mut self,
        node: NodeRef,
        other: NodeRef,
        positive_crossing: bool,
        old_oth_x: f32,
        old_oth_z: f32,
    ) {
        if self.is_entity(other) {
            self.crossed_x_entity(node, other, positive_crossing, old_oth_x, old_oth_z);
        } else {
            self.crossed_x_entity_range(node, other, positive_crossing);
        }
    }

    fn crossed_z(
        &mut self,
        node: NodeRef,
        other: NodeRef,
        positive_crossing: bool,
        old_oth_x: f32,
        old_oth_z: f32,
    ) {
        if self.is_entity(other) {
            self.crossed_z_entity(node, other, positive_crossing, old_oth_x, old_oth_z);
        } else {
            self.crossed_z_entity_range(node, other, positive_crossing);
        }
    }

    fn trigger_enter(&mut self, trigger: TriggerRef, entity_id: u32) {
        self.events
            .push(RangeListEvent::TriggerEnter { trigger, entity_id });
    }

    fn trigger_leave(&mut self, trigger: TriggerRef, entity_id: u32) {
        self.events
            .push(RangeListEvent::TriggerLeave { trigger, entity_id });
    }

    /// `RangeTriggerNode::crossedXEntity`.
    fn crossed_x_entity(
        &mut self,
        node: NodeRef,
        other: NodeRef,
        positive_crossing: bool,
        old_oth_x: f32,
        old_oth_z: f32,
    ) {
        let trigger = match self.nodes[node].kind {
            NodeKind::Bound(bound) => bound.trigger,
            _ => panic!("crossedXEntity on a non-bound node"),
        };
        if other == self.triggers[trigger].central {
            return;
        }
        // x is shuffled first so the old z position is checked.
        let old_range = self.bound(node).old_range.abs();
        if !self.was_in_z_range(trigger, old_oth_z, old_range) {
            return;
        }
        let other_entity = self.entity_id_of(other);
        let is_entering = self.is_lower_bound(node) == positive_crossing;
        if is_entering {
            let range = self.bound(node).range.abs();
            let (ox, oz) = (self.node_x(other), self.node_z(other));
            if self.is_in_x_range(trigger, ox, range) && self.is_in_z_range(trigger, oz, range) {
                self.trigger_enter(trigger, other_entity);
            }
        } else if self.was_in_x_range(trigger, old_oth_x, old_range) {
            self.trigger_leave(trigger, other_entity);
        }
    }

    /// `RangeTriggerNode::crossedZEntity`.
    fn crossed_z_entity(
        &mut self,
        node: NodeRef,
        other: NodeRef,
        positive_crossing: bool,
        old_oth_x: f32,
        old_oth_z: f32,
    ) {
        let trigger = match self.nodes[node].kind {
            NodeKind::Bound(bound) => bound.trigger,
            _ => panic!("crossedZEntity on a non-bound node"),
        };
        if other == self.triggers[trigger].central {
            return;
        }
        // z is shuffled second so the new x position is checked.
        let range = self.bound(node).range.abs();
        if !self.is_in_x_range(trigger, self.node_x(other), range) {
            return;
        }
        let other_entity = self.entity_id_of(other);
        let is_entering = self.is_lower_bound(node) == positive_crossing;
        if is_entering {
            if self.is_in_z_range(trigger, self.node_z(other), range) {
                self.trigger_enter(trigger, other_entity);
            }
        } else {
            let old_range = self.bound(node).old_range.abs();
            if self.was_in_x_range(trigger, old_oth_x, old_range)
                && self.was_in_z_range(trigger, old_oth_z, old_range)
            {
                self.trigger_leave(trigger, other_entity);
            }
        }
    }

    /// `RangeTriggerNode::crossedXEntityRange`.
    fn crossed_x_entity_range(&mut self, node: NodeRef, other: NodeRef, positive_crossing: bool) {
        let NodeKind::Bound(BoundData {
            trigger: this_trigger,
            range,
            old_range,
        }) = self.nodes[node].kind
        else {
            panic!("crossedXEntityRange on a non-bound node");
        };
        let NodeKind::Bound(BoundData {
            trigger: other_trigger,
            ..
        }) = self.nodes[other].kind
        else {
            panic!("crossedXEntityRange against a non-bound node");
        };
        let other_lower = self.triggers[other_trigger].lower;
        let other_upper = self.triggers[other_trigger].upper;

        // x is shuffled first so the old z position is checked.
        let was_in_z = self.was_in_z_range_span(
            this_trigger,
            self.bound_old_z(other_lower),
            self.bound_old_z(other_upper),
            old_range.abs(),
        );
        if !was_in_z {
            return;
        }

        let other_entity = self.entity_id_of(self.triggers[other_trigger].central);
        let is_entering = self.is_lower_bound(node) == positive_crossing;
        if is_entering {
            let is_in_x = self.is_in_x_range_span(
                this_trigger,
                self.node_x(other_lower),
                self.node_x(other_upper),
                range.abs(),
            );
            let is_in_z = self.is_in_z_range_span(
                this_trigger,
                self.node_z(other_lower),
                self.node_z(other_upper),
                range.abs(),
            );
            if is_in_x && is_in_z {
                self.trigger_enter(this_trigger, other_entity);
            }
        } else {
            let was_in_x = self.was_in_x_range_span(
                this_trigger,
                self.bound_old_x(other_lower),
                self.bound_old_x(other_upper),
                old_range.abs(),
            );
            if was_in_x {
                self.trigger_leave(this_trigger, other_entity);
            }
        }
    }

    /// `RangeTriggerNode::crossedZEntityRange`.
    fn crossed_z_entity_range(&mut self, node: NodeRef, other: NodeRef, positive_crossing: bool) {
        let NodeKind::Bound(BoundData {
            trigger: this_trigger,
            range,
            old_range,
        }) = self.nodes[node].kind
        else {
            panic!("crossedZEntityRange on a non-bound node");
        };
        let NodeKind::Bound(BoundData {
            trigger: other_trigger,
            ..
        }) = self.nodes[other].kind
        else {
            panic!("crossedZEntityRange against a non-bound node");
        };
        let other_lower = self.triggers[other_trigger].lower;
        let other_upper = self.triggers[other_trigger].upper;

        // x is shuffled first so the new x position is checked.
        let is_in_x = self.is_in_x_range_span(
            this_trigger,
            self.node_x(other_lower),
            self.node_x(other_upper),
            range.abs(),
        );
        if !is_in_x {
            return;
        }

        let other_entity = self.entity_id_of(self.triggers[other_trigger].central);
        let is_entering = self.is_lower_bound(node) == positive_crossing;
        if is_entering {
            let is_in_z = self.is_in_z_range_span(
                this_trigger,
                self.node_z(other_lower),
                self.node_z(other_upper),
                range.abs(),
            );
            if is_in_z {
                self.trigger_enter(this_trigger, other_entity);
            }
        } else {
            let was_in_x = self.was_in_x_range_span(
                this_trigger,
                self.bound_old_x(other_lower),
                self.bound_old_x(other_upper),
                old_range.abs(),
            );
            let was_in_z = self.was_in_z_range_span(
                this_trigger,
                self.bound_old_z(other_lower),
                self.bound_old_z(other_upper),
                old_range.abs(),
            );
            if was_in_x && was_in_z {
                self.trigger_leave(this_trigger, other_entity);
            }
        }
    }

    // ------------------------------------------------------------------
    // RangeTrigger range faces (geometry gates)
    // ------------------------------------------------------------------

    /// `RangeTrigger::isInXRange` (point): `(lower < x]`.
    pub fn is_in_x_range(&self, trigger: TriggerRef, x: f32, range: f32) -> bool {
        let sub_x = self.node_x(self.triggers[trigger].central);
        (sub_x - range < x) && (x <= sub_x + range)
    }

    /// `RangeTrigger::isInZRange` (point): `(lower < z]`.
    pub fn is_in_z_range(&self, trigger: TriggerRef, z: f32, range: f32) -> bool {
        let sub_z = self.node_z(self.triggers[trigger].central);
        (sub_z - range < z) && (z <= sub_z + range)
    }

    /// `RangeTrigger::wasInXRange` (point) against the central node's old spot.
    pub fn was_in_x_range(&self, trigger: TriggerRef, x: f32, range: f32) -> bool {
        let sub_x = self.triggers[trigger].old_entity_x;
        (sub_x - range < x) && (x <= sub_x + range)
    }

    /// `RangeTrigger::wasInZRange` (point) against the central node's old spot.
    pub fn was_in_z_range(&self, trigger: TriggerRef, z: f32, range: f32) -> bool {
        let sub_z = self.triggers[trigger].old_entity_z;
        (sub_z - range < z) && (z <= sub_z + range)
    }

    /// `RangeTrigger::isInXRange` (span): `[lower <= upper]` overlap.
    pub fn is_in_x_range_span(
        &self,
        trigger: TriggerRef,
        entity_lower: f32,
        entity_upper: f32,
        range: f32,
    ) -> bool {
        let sub_x = self.node_x(self.triggers[trigger].central);
        (sub_x - range <= entity_upper) && (entity_lower <= sub_x + range)
    }

    /// `RangeTrigger::isInZRange` (span) overlap.
    pub fn is_in_z_range_span(
        &self,
        trigger: TriggerRef,
        entity_lower: f32,
        entity_upper: f32,
        range: f32,
    ) -> bool {
        let sub_z = self.node_z(self.triggers[trigger].central);
        (sub_z - range <= entity_upper) && (entity_lower <= sub_z + range)
    }

    /// `RangeTrigger::wasInXRange` (span) against the central node's old spot.
    pub fn was_in_x_range_span(
        &self,
        trigger: TriggerRef,
        entity_lower: f32,
        entity_upper: f32,
        range: f32,
    ) -> bool {
        let sub_x = self.triggers[trigger].old_entity_x;
        (sub_x - range <= entity_upper) && (entity_lower <= sub_x + range)
    }

    /// `RangeTrigger::wasInZRange` (span) against the central node's old spot.
    pub fn was_in_z_range_span(
        &self,
        trigger: TriggerRef,
        entity_lower: f32,
        entity_upper: f32,
        range: f32,
    ) -> bool {
        let sub_z = self.triggers[trigger].old_entity_z;
        (sub_z - range <= entity_upper) && (entity_lower <= sub_z + range)
    }

    /// `RangeTrigger::range`.
    pub fn trigger_range(&self, trigger: TriggerRef) -> f32 {
        self.bound(self.triggers[trigger].upper).range
    }

    /// `RangeTrigger::contains( RangeListNode * )`, order-aware.
    pub fn contains(&self, trigger: TriggerRef, query: NodeRef) -> bool {
        let (qx, qz, qo) = (
            self.node_x(query),
            self.node_z(query),
            self.nodes[query].order,
        );
        let lower = self.triggers[trigger].lower;
        let upper = self.triggers[trigger].upper;
        let (lx, lz, lo) = (
            self.node_x(lower),
            self.node_z(lower),
            self.nodes[lower].order,
        );
        let (ux, uz, uo) = (
            self.node_x(upper),
            self.node_z(upper),
            self.nodes[upper].order,
        );

        (lx < qx || (lx == qx && lo < qo))
            && (qx < ux || (qx == ux && qo < uo))
            && (lz < qz || (lz == qz && lo < qo))
            && (qz < uz || (qz == uz && qo < uo))
    }

    /// `RangeTrigger::containsInZ`, order-aware.
    pub fn contains_in_z(&self, trigger: TriggerRef, query: NodeRef) -> bool {
        let (qz, qo) = (self.node_z(query), self.nodes[query].order);
        let lower = self.triggers[trigger].lower;
        let upper = self.triggers[trigger].upper;
        let (lz, lo) = (self.node_z(lower), self.nodes[lower].order);
        let (uz, uo) = (self.node_z(upper), self.nodes[upper].order);

        (lz < qz || (lz == qz && lo < qo)) && (qz < uz || (qz == uz && qo < uo))
    }

    /// `RangeTrigger::contains( float x, float z )` (point variant), used by
    /// `Space::visitLargeEntities`.
    pub fn contains_point(&self, trigger: TriggerRef, x: f32, z: f32) -> bool {
        let range = self.trigger_range(trigger);
        self.is_in_x_range(trigger, x, range) && self.is_in_z_range(trigger, z, range)
    }

    // ------------------------------------------------------------------
    // RangeTrigger lifecycle
    // ------------------------------------------------------------------

    /// `RangeTriggerNode::setRange` (bound level).
    fn bound_set_range(&mut self, node: NodeRef, range: f32) {
        let current = self.bound(node).range;
        self.bound_mut(node).old_range = current;
        let old_x = self.node_x(node);
        let old_z = self.node_z(node);
        self.bound_mut(node).range = range;
        self.shuffle_x_then_z(node, old_x, old_z);
        self.bound_mut(node).old_range = range;
    }

    /// `RangeTrigger::insert`, including the initial shuffles that pick up
    /// everything already in range.
    fn trigger_insert(&mut self, trigger: TriggerRef) {
        let (central, upper, lower) = {
            let t = &self.triggers[trigger];
            (t.central, t.upper, t.lower)
        };
        let lower_order = self.nodes[lower].order;
        let upper_order = self.nodes[upper].order;
        debug_assert!(
            lower_order < upper_order
                && lower_order > self.nodes[central].order
                && upper_order > self.nodes[central].order
        );

        let mut cursor = self.nodes[central]
            .next_x
            .expect("central node is in the list");
        while self.node_x(cursor) == self.node_x(central) && self.nodes[cursor].order < upper_order
        {
            cursor = self.nodes[cursor].next_x.expect("terminator ends the walk");
        }
        self.insert_before_x(cursor, lower);
        self.insert_before_x(cursor, upper);

        let mut cursor = self.nodes[central]
            .next_z
            .expect("central node is in the list");
        while self.node_z(cursor) == self.node_z(central) && self.nodes[cursor].order < upper_order
        {
            cursor = self.nodes[cursor].next_z.expect("terminator ends the walk");
        }
        self.insert_before_z(cursor, lower);
        self.insert_before_z(cursor, upper);

        let (old_entity_x, old_entity_z) = (self.node_x(central), self.node_z(central));
        self.triggers[trigger].old_entity_x = old_entity_x;
        self.triggers[trigger].old_entity_z = old_entity_z;

        // now perform the initial shuffle from this location, which is
        // the same spot that setRange(0) would put us in
        self.bound_mut(upper).old_range = 0.0;
        self.bound_mut(lower).old_range = 0.0;

        self.shuffle_x(upper, old_entity_x, old_entity_z);
        self.shuffle_x(lower, old_entity_x, old_entity_z);

        self.shuffle_z(upper, old_entity_x, old_entity_z);
        self.shuffle_z(lower, old_entity_x, old_entity_z);

        let upper_range = self.bound(upper).range;
        let lower_range = self.bound(lower).range;
        self.bound_mut(upper).old_range = upper_range;
        self.bound_mut(lower).old_range = lower_range;
    }

    /// `RangeTrigger::remove`: contract to a point, unlink, restore ranges.
    fn trigger_remove(&mut self, trigger: TriggerRef) {
        let (upper, lower) = {
            let t = &self.triggers[trigger];
            (t.upper, t.lower)
        };
        let our = self.bound(upper).range;
        let olr = self.bound(lower).range;
        self.bound_set_range(upper, 0.0);
        self.bound_set_range(lower, 0.0);
        self.remove_from_range_list(upper);
        self.remove_from_range_list(lower);
        self.bound_mut(upper).range = our;
        self.bound_mut(lower).range = olr;
    }

    /// `RangeTrigger` construction for the script triggers
    /// (`proximity_controller.cpp:24-30`, `vision_controller.cpp:22-32`):
    /// both bounds want entity crossings and make nothing. The trigger is
    /// inserted (the initial shuffles fire any enters) and registered on
    /// its entity.
    pub fn add_script_trigger(&mut self, entity_id: u32, range: f32) -> TriggerRef {
        let central = self.entity_nodes[&entity_id];
        let trigger = self.triggers.len();
        let upper = self.new_bound_node(trigger, range, FLAG_ENTITY_TRIGGER, FLAG_NO_TRIGGERS);
        // RangeTrigger's ctor ORs FLAG_IS_LOWER_BOUND into the lower
        // bound's makes flags (which also gives it the lower-bound order).
        let lower = self.new_bound_node(trigger, -range, FLAG_ENTITY_TRIGGER, FLAG_IS_LOWER_BOUND);
        let (x, z) = self.entity_position(central);
        self.triggers.push(RangeTriggerData {
            central,
            upper,
            lower,
            old_entity_x: x,
            old_entity_z: z,
        });
        self.trigger_insert(trigger);
        self.add_trigger(entity_id, trigger);
        trigger
    }

    /// The script trigger destruction (`proximity_controller.cpp:37-41`,
    /// `vision_controller.cpp:34-46`): `Entity::delTrigger` then the
    /// contracting `RangeTrigger::remove` — the contraction fires the
    /// leave events.
    pub fn remove_script_trigger(&mut self, entity_id: u32, trigger: TriggerRef) {
        self.del_trigger(entity_id, trigger);
        self.trigger_remove(trigger);
    }

    /// Whether the entity is on this app (`CellApp::findEntity` for the
    /// onload reconciliation paths).
    pub fn has_entity(&self, entity_id: u32) -> bool {
        self.entity_nodes.contains_key(&entity_id)
    }

    /// The trapped set: the entity ids between the trigger's bounds in x
    /// order, inside its z range, other than the central entity
    /// (`ProximityController::writeRealToStream`'s walk,
    /// `proximity_controller.cpp:311-345`).
    pub fn entities_between(&self, trigger: TriggerRef) -> Vec<u32> {
        let t = &self.triggers[trigger];
        let (central, upper, lower) = (t.central, t.upper, t.lower);
        let mut out = Vec::new();
        let mut cursor = self.nodes[lower].next_x;
        while let Some(node) = cursor {
            if node == upper {
                break;
            }
            if let NodeKind::Entity { entity_id, .. } = self.nodes[node].kind
                && node != central
                && self.contains_in_z(trigger, node)
            {
                out.push(entity_id);
            }
            cursor = self.nodes[node].next_x;
        }
        out
    }

    /// `RangeTrigger::removeWithoutContracting`.
    pub fn remove_trigger_without_contracting(&mut self, trigger: TriggerRef) {
        let (upper, lower) = {
            let t = &self.triggers[trigger];
            (t.upper, t.lower)
        };
        self.remove_from_range_list(upper);
        self.remove_from_range_list(lower);
    }

    /// `RangeTrigger::shuffleXThenZExpand`.
    fn trigger_shuffle_x_then_z_expand(
        &mut self,
        trigger: TriggerRef,
        x_inc: bool,
        z_inc: bool,
        old_x: f32,
        old_z: f32,
    ) {
        let (upper, lower) = {
            let t = &self.triggers[trigger];
            (t.upper, t.lower)
        };
        let x_trigger = if x_inc { upper } else { lower };
        let r = self.bound(x_trigger).range;
        self.shuffle_x(x_trigger, old_x + r, old_z + r);
        let z_trigger = if z_inc { upper } else { lower };
        let r = self.bound(z_trigger).range;
        self.shuffle_z(z_trigger, old_x + r, old_z + r);
    }

    /// `RangeTrigger::shuffleXThenZContract`.
    fn trigger_shuffle_x_then_z_contract(
        &mut self,
        trigger: TriggerRef,
        x_inc: bool,
        z_inc: bool,
        old_x: f32,
        old_z: f32,
    ) {
        let (central, upper, lower) = {
            let t = &self.triggers[trigger];
            (t.central, t.upper, t.lower)
        };
        let x_trigger = if x_inc { lower } else { upper };
        let r = self.bound(x_trigger).range;
        self.shuffle_x(x_trigger, old_x + r, old_z + r);
        let z_trigger = if z_inc { lower } else { upper };
        let r = self.bound(z_trigger).range;
        self.shuffle_z(z_trigger, old_x + r, old_z + r);
        self.triggers[trigger].old_entity_x = self.node_x(central);
        self.triggers[trigger].old_entity_z = self.node_z(central);
    }

    /// `RangeTrigger::setRange` (clamped to at least 0.001).
    pub fn set_trigger_range(&mut self, trigger: TriggerRef, range: f32) {
        let mut r = range;
        if r < 0.001 {
            r = 0.001;
        }
        let (upper, lower) = {
            let t = &self.triggers[trigger];
            (t.upper, t.lower)
        };
        self.bound_set_range(upper, r);
        self.bound_set_range(lower, -r);
    }

    // ------------------------------------------------------------------
    // Entity-level faces
    // ------------------------------------------------------------------

    /// `Entity::addToRangeList` without an AppealRadius: add the entity's node
    /// to the range list (the initial shuffle announces any enters).
    pub fn add_entity(&mut self, entity_id: u32, x: f32, z: f32) -> NodeRef {
        let node = self.nodes.len();
        self.nodes.push(RangeNode {
            prev_x: None,
            next_x: None,
            prev_z: None,
            next_z: None,
            wants_flags: FLAG_NO_TRIGGERS,
            makes_flags: FLAG_ENTITY_TRIGGER
                | FLAG_LOWER_AOI_TRIGGER
                | FLAG_UPPER_AOI_TRIGGER
                | FLAG_IS_ENTITY,
            order: RANGE_LIST_ORDER_ENTITY,
            kind: NodeKind::Entity { entity_id, x, z },
        });
        self.entity_nodes.insert(entity_id, node);
        self.range_list_add(node);
        node
    }

    /// `RangeList::add`.
    fn range_list_add(&mut self, node: NodeRef) {
        let first_next_x = self.nodes[self.head].next_x.expect("head is never last");
        self.insert_before_x(first_next_x, node);
        let first_next_z = self.nodes[self.head].next_z.expect("head is never last");
        self.insert_before_z(first_next_z, node);
        self.shuffle_x_then_z(node, -f32::MAX, -f32::MAX);
    }

    /// `Entity::addTrigger`: keep `triggers_` sorted descending by range.
    pub fn add_trigger(&mut self, entity_id: u32, trigger: TriggerRef) {
        let new_range = self.trigger_range(trigger);
        let existing: Vec<f32> = self
            .triggers_of_entity
            .get(&entity_id)
            .map(|list| list.iter().map(|&t| self.trigger_range(t)).collect())
            .unwrap_or_default();
        let pos = existing
            .iter()
            .position(|&r| r <= new_range)
            .unwrap_or(existing.len());
        let list = self.triggers_of_entity.entry(entity_id).or_default();
        list.insert(pos, trigger);
    }

    /// `Entity::delTrigger`.
    pub fn del_trigger(&mut self, entity_id: u32, trigger: TriggerRef) {
        let list = self.triggers_of_entity.entry(entity_id).or_default();
        let pos = list
            .iter()
            .position(|&t| t == trigger)
            .expect("MF_ASSERT( found != triggers_.end() )");
        list.remove(pos);
    }

    /// `Entity::modTrigger`.
    pub fn mod_trigger(&mut self, entity_id: u32, trigger: TriggerRef) {
        self.del_trigger(entity_id, trigger);
        self.add_trigger(entity_id, trigger);
    }

    /// `Entity::triggers_` readback (sorted descending by range).
    pub fn triggers_of(&self, entity_id: u32) -> &[TriggerRef] {
        self.triggers_of_entity
            .get(&entity_id)
            .map(|v| v.as_slice())
            .unwrap_or(&[])
    }

    /// `AoITrigger` construction (witness.cpp): wants the AOI flags so that
    /// entity nodes and appeal bounds announce crossings to its bounds. The
    /// constructor first collects the large entities whose appeal range we
    /// currently sit in (`Space::visitLargeEntities`), then inserts and
    /// registers itself as one of the owner's moving triggers.
    pub fn add_aoi_trigger(&mut self, owner_entity_id: u32, range: f32) -> TriggerRef {
        let central = self.entity_nodes[&owner_entity_id];
        let trigger = self.triggers.len();
        let upper = self.new_bound_node(trigger, range, FLAG_UPPER_AOI_TRIGGER, FLAG_NO_TRIGGERS);
        // RangeTrigger's ctor ORs FLAG_IS_LOWER_BOUND into the lower bound's
        // makes flags (which also gives it the lower-bound list order).
        let lower =
            self.new_bound_node(trigger, -range, FLAG_LOWER_AOI_TRIGGER, FLAG_IS_LOWER_BOUND);
        let (x, z) = self.entity_position(central);
        self.triggers.push(RangeTriggerData {
            central,
            upper,
            lower,
            old_entity_x: x,
            old_entity_z: z,
        });

        // Space::visitLargeEntities( x, z, *this )
        let appeal_triggers = self.appeal_triggers.clone();
        for appeal in appeal_triggers {
            if self.contains_point(appeal, x, z) {
                let seen = self.entity_id_of(self.triggers[appeal].central);
                self.trigger_enter(trigger, seen);
            }
        }

        self.trigger_insert(trigger);
        self.add_trigger(owner_entity_id, trigger);
        trigger
    }

    /// `RangeListAppealTrigger` construction plus the appeal half of
    /// `Entity::addToRangeList`: the entity node stops being an AOI trigger
    /// (its appeal bounds take over making the AOI flags), the trigger is
    /// inserted and joins the moving triggers and the space's appeal list.
    /// The ctor swaps the makes flags: the upper edge makes LOWER_AOI and the
    /// lower edge makes UPPER_AOI, so a witness bound pairs with the FAR
    /// appeal edge crossing it.
    pub fn add_appeal_trigger(&mut self, entity_id: u32, range: f32) -> TriggerRef {
        let central = self.entity_nodes[&entity_id];
        let trigger = self.triggers.len();
        let upper = self.new_bound_node(trigger, range, FLAG_NO_TRIGGERS, FLAG_LOWER_AOI_TRIGGER);
        let lower = self.new_bound_node(
            trigger,
            -range,
            FLAG_NO_TRIGGERS,
            FLAG_UPPER_AOI_TRIGGER | FLAG_IS_LOWER_BOUND,
        );
        let (x, z) = self.entity_position(central);
        self.triggers.push(RangeTriggerData {
            central,
            upper,
            lower,
            old_entity_x: x,
            old_entity_z: z,
        });

        // pRangeListNode_->isAoITrigger( false ): the entity node stops
        // making the AOI flags while the appeal bounds speak for it.
        self.nodes[central].makes_flags &= !(FLAG_LOWER_AOI_TRIGGER | FLAG_UPPER_AOI_TRIGGER);

        self.trigger_insert(trigger);
        self.add_trigger(entity_id, trigger);
        self.appeal_triggers.push(trigger);
        trigger
    }

    /// The appeal half of `Entity::removeFromRangeList`: drop the trigger from
    /// the moving triggers and the space's appeal list, remove it (with
    /// contraction) and restore the entity node's AOI-trigger flag.
    pub fn remove_appeal_trigger(&mut self, entity_id: u32) {
        let pos = self
            .appeal_triggers
            .iter()
            .position(|&t| self.triggers[t].central == self.entity_nodes[&entity_id])
            .expect("entity has an appeal trigger");
        let trigger = self.appeal_triggers.remove(pos);
        self.del_trigger(entity_id, trigger);
        self.trigger_remove(trigger);

        let central = self.entity_nodes[&entity_id];
        // isAoITrigger( true ): the entity node resumes making the AOI flags.
        self.nodes[central].makes_flags |= FLAG_LOWER_AOI_TRIGGER | FLAG_UPPER_AOI_TRIGGER;
    }

    fn new_bound_node(
        &mut self,
        trigger: TriggerRef,
        range: f32,
        wants_flags: u16,
        makes_flags: u16,
    ) -> NodeRef {
        let node = self.nodes.len();
        self.nodes.push(RangeNode {
            prev_x: None,
            next_x: None,
            prev_z: None,
            next_z: None,
            wants_flags,
            makes_flags,
            order: if makes_flags & FLAG_IS_LOWER_BOUND != 0 {
                RANGE_LIST_ORDER_LOWER_BOUND
            } else {
                RANGE_LIST_ORDER_UPPER_BOUND
            },
            kind: NodeKind::Bound(BoundData {
                trigger,
                range,
                old_range: 0.0,
            }),
        });
        node
    }

    /// `Entity::updateInternalsForNewPosition`: shuffle the leading triggers,
    /// the entity, then the trailing triggers.
    pub fn move_entity(&mut self, entity_id: u32, x: f32, z: f32) {
        let node = self.entity_nodes[&entity_id];
        let (old_x, old_z) = self.entity_position(node);
        self.set_entity_position(node, x, z);

        let increase_x = old_x < x;
        let increase_z = old_z < z;

        let triggers = self
            .triggers_of_entity
            .get(&entity_id)
            .cloned()
            .unwrap_or_default();
        for trigger in &triggers {
            self.trigger_shuffle_x_then_z_expand(*trigger, increase_x, increase_z, old_x, old_z);
        }
        self.shuffle_x_then_z(node, old_x, old_z);
        for trigger in triggers.iter().rev() {
            self.trigger_shuffle_x_then_z_contract(*trigger, increase_x, increase_z, old_x, old_z);
        }
    }

    /// `EntityRangeListNode::remove`: park the node at maximum z, shuffle (so
    /// observers lose it) and unlink from both lists.
    pub fn remove_entity(&mut self, entity_id: u32) {
        let node = self.entity_nodes[&entity_id];
        let (x, old_z) = self.entity_position(node);
        self.set_entity_position(node, x, f32::MAX);
        self.shuffle_z(node, x, old_z);
        self.remove_from_range_list(node);
    }

    /// `Entity::isAoITrigger` readback for tests.
    /// Readback: the entity is an AoI trigger iff its node still makes the
    /// AOI flags (they are stripped while appeal bounds speak for it).
    pub fn is_aoi_trigger(&self, entity_id: u32) -> bool {
        let node = self.entity_nodes[&entity_id];
        self.nodes[node].makes_flags & (FLAG_LOWER_AOI_TRIGGER | FLAG_UPPER_AOI_TRIGGER) != 0
    }

    /// `EntityRangeListNode::x/z` readback for tests.
    pub fn position(&self, entity_id: u32) -> (f32, f32) {
        self.entity_position(self.entity_nodes[&entity_id])
    }

    // ------------------------------------------------------------------
    // Debug faces
    // ------------------------------------------------------------------

    /// `RangeList::isSorted`.
    pub fn is_sorted(&self) -> bool {
        let mut prev = self.head;
        let mut iter = self.nodes[self.head].next_x;
        while let Some(cur) = iter {
            if self.node_x(prev) > self.node_x(cur) {
                return false;
            }
            prev = cur;
            iter = self.nodes[cur].next_x;
        }

        let mut prev = self.head;
        let mut iter = self.nodes[self.head].next_z;
        while let Some(cur) = iter {
            if self.node_z(prev) > self.node_z(cur) {
                return false;
            }
            prev = cur;
            iter = self.nodes[cur].next_z;
        }
        true
    }

    /// X-axis list contents from the head, for tests.
    pub fn x_chain(&self) -> Vec<NodeRef> {
        let mut out = Vec::new();
        let mut iter = self.nodes[self.head].next_x;
        while let Some(cur) = iter {
            out.push(cur);
            iter = self.nodes[cur].next_x;
        }
        out
    }
}

impl Default for RangeListWorld {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn enter(trigger: TriggerRef, entity_id: u32) -> RangeListEvent {
        RangeListEvent::TriggerEnter { trigger, entity_id }
    }

    fn leave(trigger: TriggerRef, entity_id: u32) -> RangeListEvent {
        RangeListEvent::TriggerLeave { trigger, entity_id }
    }

    /// Walking straight through the radius announces exactly one enter at the
    /// lower edge and one leave at the upper edge, both driven by list
    /// crossings and validated by the geometry gates.
    #[test]
    fn a_walker_crossing_the_radius_enters_and_leaves() {
        let mut world = RangeListWorld::new();
        world.add_entity(1, 0.0, 0.0);
        world.add_aoi_trigger(1, 10.0);
        let walker = world.add_entity(2, -20.0, 0.0);
        assert!(world.events.is_empty());

        // crosses the lower bound (walker -> bound reverse call, entering)
        world.move_entity(2, -9.0, 0.0);
        assert_eq!(world.events, vec![enter(0, 2)]);
        world.events.clear();
        assert!(world.contains(0, walker));
        assert!(world.contains_in_z(0, walker));

        world.move_entity(2, 9.0, 0.0);
        assert!(world.events.is_empty());
        world.move_entity(2, 9.5, 0.0);
        assert!(world.events.is_empty());

        // exactly on the upper bound: the entity node (order 100) sorts
        // before the upper bound (order 200), so no crossing fires and the
        // half-open (lower, upper] window still contains it
        world.move_entity(2, 10.0, 0.0);
        assert!(world.events.is_empty());

        world.move_entity(2, 10.5, 0.0);
        assert_eq!(world.events, vec![leave(0, 2)]);
        world.events.clear();
        assert!(!world.contains(0, walker));

        world.move_entity(2, 30.0, 0.0);
        assert!(world.events.is_empty());

        world.move_entity(2, 9.0, 0.0);
        assert_eq!(world.events, vec![enter(0, 2)]);
        world.events.clear();

        // exits through the lower bound moving left
        world.move_entity(2, -11.0, 0.0);
        assert_eq!(world.events, vec![leave(0, 2)]);
        assert!(world.is_sorted());
    }

    /// The lower boundary is exclusive: standing on it fires nothing, and the
    /// order tiebreak only announces a leave once the entity has actually
    /// been inside.
    #[test]
    fn the_lower_boundary_is_exclusive_until_actually_crossed() {
        let mut world = RangeListWorld::new();
        world.add_entity(1, 0.0, 0.0);
        world.add_aoi_trigger(1, 10.0);

        // added exactly on the lower bound: equal coordinates keep the entity
        // node (order 100) ahead of the lower bound (order 190)
        world.add_entity(2, -10.0, 0.0);
        assert!(world.events.is_empty());
        assert!(!world.is_in_x_range(0, -10.0, 10.0));

        // a strictly positive step past the bound announces the enter
        world.move_entity(2, -9.9999, 0.0);
        assert_eq!(world.events, vec![enter(0, 2)]);
        world.events.clear();

        // stepping back onto the exact bound: the tiebreak shuffles the
        // entity node ahead of the bound, which fires the leave (the entity
        // is now outside the half-open window)
        world.move_entity(2, -10.0, 0.0);
        assert_eq!(world.events, vec![leave(0, 2)]);
        world.events.clear();

        // leaving further: the wasInX gate rejects the exact-bound old spot
        world.move_entity(2, -10.5, 0.0);
        assert!(world.events.is_empty());
    }

    /// Z crossings gate the enter on the NEW x and the leave on the OLD pair.
    #[test]
    fn z_axis_crossings_gate_on_the_new_x_and_the_old_pair() {
        let mut world = RangeListWorld::new();
        world.add_entity(1, 0.0, 0.0);
        world.add_aoi_trigger(1, 10.0);
        world.add_entity(2, 5.0, 20.0);
        assert!(world.events.is_empty());

        world.move_entity(2, 5.0, -1.0);
        assert_eq!(world.events, vec![enter(0, 2)]);
        world.events.clear();

        world.move_entity(2, 5.0, 20.0);
        assert_eq!(world.events, vec![leave(0, 2)]);
    }

    /// RangeList::add performs the initial shuffle from -FLT_MAX, so an
    /// entity added inside the radius enters immediately. The x sweep's
    /// crossing is gated out by the -FLT_MAX wasInZ payload; the single
    /// enter comes from the z sweep.
    #[test]
    fn a_new_entity_added_inside_the_radius_enters_immediately() {
        let mut world = RangeListWorld::new();
        world.add_entity(1, 0.0, 0.0);
        world.add_aoi_trigger(1, 10.0);

        world.add_entity(2, 5.0, 0.0);
        assert_eq!(world.events, vec![enter(0, 2)]);
        world.events.clear();

        // added outside, both sweeps are rejected by the gates
        world.add_entity(3, 15.0, 0.0);
        assert!(world.events.is_empty());
    }

    /// RangeTrigger::setRange contracts (leaving whoever falls out) and
    /// re-expands (re-entering them), via the bound-level setRange shuffles.
    #[test]
    fn set_range_contracts_and_re_expands_the_trigger() {
        let mut world = RangeListWorld::new();
        world.add_entity(1, 0.0, 0.0);
        world.add_aoi_trigger(1, 10.0);
        world.add_entity(2, -7.0, 0.0);
        world.add_entity(3, 7.0, 0.0);
        assert_eq!(world.events, vec![enter(0, 2), enter(0, 3)]);
        world.events.clear();

        world.set_trigger_range(0, 5.0);
        assert_eq!(world.events, vec![leave(0, 3), leave(0, 2)]);
        world.events.clear();
        assert_eq!(world.trigger_range(0), 5.0);

        world.set_trigger_range(0, 10.0);
        assert_eq!(world.events, vec![enter(0, 3), enter(0, 2)]);
    }

    /// EntityRangeListNode::remove parks the node at FLT_MAX z; the bound it
    /// flies through announces the leave with the old-position gates.
    #[test]
    fn removing_an_entity_announces_leaves_via_the_z_flight() {
        let mut world = RangeListWorld::new();
        world.add_entity(1, 0.0, 0.0);
        world.add_aoi_trigger(1, 10.0);
        world.add_entity(2, 5.0, -5.0);
        assert_eq!(world.events, vec![enter(0, 2)]);
        world.events.clear();

        world.remove_entity(2);
        assert_eq!(world.events, vec![leave(0, 2)]);
        assert!(world.is_sorted());
    }

    /// The observer moving moves its bounds: the leading edge expands first,
    /// the trailing edge contracts after the entity shuffle.
    #[test]
    fn the_observer_moving_moves_its_bounds() {
        let mut world = RangeListWorld::new();
        world.add_entity(1, 0.0, 0.0);
        world.add_entity(2, 20.0, 0.0);
        world.add_aoi_trigger(1, 10.0);
        assert!(world.events.is_empty());

        world.move_entity(1, 25.0, 0.0);
        assert_eq!(world.events, vec![enter(0, 2)]);
        world.events.clear();

        world.move_entity(1, 0.0, 0.0);
        assert_eq!(world.events, vec![leave(0, 2)]);
    }

    /// A witness moving towards a large entity enters when its expanding
    /// upper bound crosses the appeal's lower edge (which makes UPPER_AOI),
    /// and leaves when the contracting upper bound sweeps back.
    #[test]
    fn a_witness_approaching_a_large_entity_enters_and_leaves() {
        let mut world = RangeListWorld::new();
        world.add_entity(1, 0.0, 0.0);
        world.add_entity(2, 30.0, 0.0);
        world.add_appeal_trigger(2, 15.0);
        world.add_aoi_trigger(1, 10.0);
        assert!(world.events.is_empty());

        world.move_entity(1, 12.0, 0.0);
        assert_eq!(world.events, vec![enter(1, 2)]);
        world.events.clear();

        world.move_entity(1, 0.0, 0.0);
        assert_eq!(world.events, vec![leave(1, 2)]);
    }

    /// AppealRadius: the entity node stops making AOI crossings; the appeal
    /// bounds take over with swapped makes flags (upper edge makes LOWER_AOI,
    /// lower edge makes UPPER_AOI), so a witness bound pairs with the far
    /// appeal edge crossing it.
    #[test]
    fn an_appeal_radius_takes_over_crossing_making_for_its_entity() {
        let mut world = RangeListWorld::new();
        world.add_entity(1, 0.0, 0.0);
        world.add_aoi_trigger(1, 10.0);
        world.add_entity(2, -25.0, 0.0);
        assert!(world.events.is_empty());

        world.add_appeal_trigger(2, 5.0);
        assert!(!world.is_aoi_trigger(2));
        assert!(world.events.is_empty());

        // the appeal's upper edge (x -20 -> -3) sweeps over the witness's
        // lower bound at -10 and announces the overlap
        world.move_entity(2, -8.0, 0.0);
        assert_eq!(world.events, vec![enter(0, 2)]);
        world.events.clear();

        // both edges stay inside the witness range: nothing crosses
        world.move_entity(2, -4.0, 0.0);
        assert!(world.events.is_empty());

        // the appeal's lower edge (x -9 -> 15) contracts past the witness's
        // upper bound at 10
        world.move_entity(2, 20.0, 0.0);
        assert_eq!(world.events, vec![leave(0, 2)]);
        world.events.clear();

        // removing the appeal restores the entity node's AOI-trigger flag
        world.remove_appeal_trigger(2);
        assert!(world.is_aoi_trigger(2));
        assert!(world.events.is_empty());

        // so its own node crossings reach the witness bounds again
        world.move_entity(2, 5.0, 0.0);
        assert_eq!(world.events, vec![enter(0, 2)]);
    }

    /// AoITrigger construction first visits the large entities whose appeal
    /// range contains the witness point, then the insert shuffles sweep up
    /// whatever the edges cross.
    #[test]
    fn creating_an_aoi_trigger_visits_large_entities_first() {
        let mut world = RangeListWorld::new();
        world.add_entity(1, 30.0, 0.0);
        world.add_appeal_trigger(1, 25.0);
        world.add_entity(2, 10.0, 0.0);
        assert!(world.events.is_empty());

        let aoi = world.add_aoi_trigger(2, 10.0);
        // the only enter comes from visitLargeEntities (the point (10, 0)
        // sits inside the appeal span [5, 55] x [-25, 25]); the insert's
        // initial sweeps are gated out by the zero oldRange payloads
        assert_eq!(world.events, vec![enter(aoi, 1)]);
    }

    /// Entity::modTrigger re-sorts the moving trigger list after a range
    /// change (descending by range).
    #[test]
    fn mod_trigger_resorts_the_moving_trigger_list() {
        let mut world = RangeListWorld::new();
        world.add_entity(1, 0.0, 0.0);
        let aoi = world.add_aoi_trigger(1, 5.0);
        let appeal = world.add_appeal_trigger(1, 20.0);
        assert_eq!(world.triggers_of(1), &[appeal, aoi]);

        world.set_trigger_range(aoi, 30.0);
        world.events.clear();
        world.mod_trigger(1, aoi);
        assert_eq!(world.triggers_of(1), &[aoi, appeal]);
        assert_eq!(world.trigger_range(aoi), 30.0);
    }

    /// The is/was range faces reproduce the reference boundaries and read the
    /// central node's current vs previous spot; setRange clamps to 0.001.
    #[test]
    fn the_range_faces_use_the_reference_boundaries() {
        let mut world = RangeListWorld::new();
        world.add_entity(1, 100.0, 100.0);
        let aoi = world.add_aoi_trigger(1, 10.0);

        // point faces: (lower, upper]
        assert!(!world.is_in_x_range(aoi, 90.0, 10.0));
        assert!(world.is_in_x_range(aoi, 90.5, 10.0));
        assert!(world.is_in_x_range(aoi, 110.0, 10.0));
        assert!(!world.is_in_x_range(aoi, 110.5, 10.0));

        // span faces: [lower <= eU && eL <= upper]
        assert!(world.is_in_x_range_span(aoi, 105.0, 115.0, 10.0));
        assert!(!world.is_in_x_range_span(aoi, 110.5, 115.0, 10.0));

        world.move_entity(1, 105.0, 100.0);
        assert!(world.events.is_empty());
        assert_eq!(world.position(1), (105.0, 100.0));
        assert!(world.was_in_x_range(aoi, 100.0, 10.0));
        assert!(!world.was_in_x_range(aoi, 120.0, 10.0));
        assert!(world.is_in_x_range(aoi, 110.0, 10.0));
        assert!(!world.is_in_x_range(aoi, 95.0, 10.0));

        world.set_trigger_range(aoi, 0.0);
        assert_eq!(world.trigger_range(aoi), 0.001);
    }

    /// removeWithoutContracting unlinks the bounds without any shuffles, so
    /// no leaves fire and later movement is unaffected.
    #[test]
    fn removing_without_contracting_drops_the_bounds_silently() {
        let mut world = RangeListWorld::new();
        world.add_entity(1, 0.0, 0.0);
        let aoi = world.add_aoi_trigger(1, 10.0);
        world.add_entity(2, 5.0, 0.0);
        world.events.clear();

        world.remove_trigger_without_contracting(aoi);
        assert!(world.events.is_empty());

        world.move_entity(2, -20.0, 0.0);
        assert!(world.events.is_empty());
        assert!(world.is_sorted());
    }

    /// Same-coordinate entities keep their insertion order (equal order
    /// values stop the shuffle); the chain stays sorted.
    #[test]
    fn entities_settle_into_a_sorted_chain_with_order_tiebreaks() {
        let mut world = RangeListWorld::new();
        world.add_entity(1, 0.0, 0.0);
        world.add_entity(2, 0.0, 0.0);
        world.add_entity(3, 0.0, 0.0);
        assert!(world.is_sorted());
        // head and tail terminators occupy slab slots 0 and 1
        assert_eq!(world.x_chain(), vec![2, 3, 4, 1]);

        // adding a trigger walks the cursor past the same-coordinate
        // entities (equal coords, lower order) before inserting; the lower
        // z bound's initial sweep then enters the stacked entities (they
        // stand inside the (lower, upper] window of the fresh trigger)
        world.add_aoi_trigger(1, 10.0);
        assert_eq!(world.events, vec![enter(0, 3), enter(0, 2)]);
        world.events.clear();
        assert!(world.is_sorted());

        let _world = RangeListWorld::default();
    }

    /// A witness moving in z sweeps its bounds over the other entity: the
    /// bound's own crossedZEntity (self-call) announces the enter on the way
    /// up and the leave on the way back.
    #[test]
    fn a_witness_sweeping_z_over_an_entity_self_calls_the_pairing() {
        let mut world = RangeListWorld::new();
        world.add_entity(1, 0.0, 0.0);
        world.add_aoi_trigger(1, 10.0);
        world.add_entity(2, 5.0, 20.0);
        assert!(world.events.is_empty());

        // the expanding upper z bound sweeps past the entity (entering)
        world.move_entity(1, 0.0, 15.0);
        assert_eq!(world.events, vec![enter(0, 2)]);
        world.events.clear();

        // the contracting upper z bound sweeps back (leaving)
        world.move_entity(1, 0.0, 0.0);
        assert_eq!(world.events, vec![leave(0, 2)]);
    }

    /// A large entity moving in z drives its appeal bounds across the
    /// witness bounds as range-to-range crossings: the witness lower-side
    /// bound pairs with the appeal's lower edge (which makes LOWER_AOI plus
    /// IS_LOWER_BOUND) for the enter, and the witness upper-side bound with
    /// the same edge for the leave.
    #[test]
    fn an_appeal_moving_in_z_crosses_the_witness_bounds_as_ranges() {
        let mut world = RangeListWorld::new();
        world.add_entity(1, 0.0, 0.0);
        world.add_aoi_trigger(1, 10.0);
        world.add_entity(2, 5.0, 40.0);
        world.add_appeal_trigger(2, 15.0);
        assert!(world.events.is_empty());

        // the expanding lower z edge sweeps down over the witness's z bounds
        world.move_entity(2, 5.0, 0.0);
        assert_eq!(world.events, vec![enter(0, 2)]);
        world.events.clear();

        // the contracting lower z edge sweeps back up
        world.move_entity(2, 5.0, 40.0);
        assert_eq!(world.events, vec![leave(0, 2)]);
    }

    #[test]
    fn a_z_sweep_over_a_disjoint_entity_is_silent() {
        let mut world = RangeListWorld::new();
        world.add_entity(1, 0.0, 0.0);
        world.add_aoi_trigger(1, 10.0);
        world.add_entity(2, 30.0, 40.0);
        world.add_appeal_trigger(2, 15.0);
        assert!(world.events.is_empty());

        // x spans never overlap: the z bounds sweep down and back up past the
        // witness, but crossedZEntityRange rejects on the x gate up front.
        world.move_entity(2, 30.0, 0.0);
        world.move_entity(2, 30.0, 40.0);
        assert!(world.events.is_empty());
    }
}
