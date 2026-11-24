//! `lib/waypoint/chunk_waypoint_set_state.{hpp,cpp}` — the set-level A*
//! state: one waypoint set per state, walking the portal connections.
//!
//! This is what `Navigator::searchNextWaypointSet` searches over to find
//! the sequence of chunk crossings; the per-edge detail is the chunk-level
//! `ChunkWaypointState` search that follows it. The gates in
//! `getAdjacency` run in the reference's order — non-permissive block,
//! one-way portal refusal (via the reciprocal connection), shell boundary
//! and activated-portal flags, the chunk-gone guard, and the position
//! choice.
//!
//! Registered boundaries (same answers, different bookkeeping):
//! - the reference finds the back portal by scanning the destination
//!   chunk's boundary portals (`pbegin()..pend()`); the port reads the
//!   destination set's reciprocal connection portal instead. The scan's
//!   only observable effects are the `false` it answers (subsumed: a
//!   boundary scan miss implies no reciprocal connection either) and the
//!   `hasChunkItem` it feeds the activated-portal flag — which the
//!   reciprocal connection carries too.
//! - the chunk identity each set sits on is the data record's optional id
//!   (see `chunk_waypoint_set.rs`); `chunk()`-gone answers false for an
//!   unmounted set, and the shell formula compares ids.
//! - the reference's `ChunkWPSetState(pSet)` constructor positions the
//!   state at `chunk()->centre()`; that needs the chunk mounting and
//!   waits for the chunk lane. The port offers the default and `NavLoc`
//!   constructors, the pair this ticket's faces consume.

use super::astar::AStarState;
use super::chunk_waypoint_set_data::ChunkWaypointSetData;
use super::navloc::NavLoc;
use foundation::math::Vector3;
use std::rc::Rc;

/// `ChunkWPSetState` (`chunk_waypoint_set_state.hpp:26-77`): one state in
/// an A* search of the waypoint-set graph.
#[derive(Debug, Clone, Default)]
pub struct ChunkWPSetState {
    /// `pSet_`.
    p_set: Option<Rc<ChunkWaypointSetData>>,
    /// `blockNonPermissive_`.
    block_non_permissive: bool,
    /// `distanceFromParent_`.
    distance_from_parent: f32,
    /// `passedActivatedPortal_`.
    passed_activated_portal: bool,
    /// `passedShellBoundary_`.
    passed_shell_boundary: bool,
    /// `position_`.
    position: Vector3,
}

impl ChunkWPSetState {
    /// `ChunkWPSetState( const NavLoc & loc )`
    /// (`chunk_waypoint_set_state.cpp:45-53`).
    pub fn from_navloc(nav_loc: &NavLoc) -> Self {
        Self {
            p_set: nav_loc.p_set().cloned(),
            block_non_permissive: false,
            distance_from_parent: 0.0,
            passed_activated_portal: false,
            passed_shell_boundary: false,
            position: nav_loc.point(),
        }
    }

    /// `blockNonPermissive( bool )` (`chunk_waypoint_set_state.hpp:81-84`).
    pub fn set_block_non_permissive(&mut self, block: bool) {
        self.block_non_permissive = block;
    }

    /// `pSet()` (`chunk_waypoint_set_state.hpp:55-56`).
    pub fn p_set(&self) -> Option<&Rc<ChunkWaypointSetData>> {
        self.p_set.as_ref()
    }

    /// `position()` (`chunk_waypoint_set_state.hpp:73-74`).
    pub fn position(&self) -> Vector3 {
        self.position
    }

    /// `passedShellBoundary()`
    /// (`chunk_waypoint_set_state.hpp:87-88`).
    pub fn passed_shell_boundary(&self) -> bool {
        self.passed_shell_boundary
    }

    /// `passedActivatedPortal()`
    /// (`chunk_waypoint_set_state.hpp:65-66`).
    pub fn passed_activated_portal(&self) -> bool {
        self.passed_activated_portal
    }

    /// `desc` (`chunk_waypoint_set_state.cpp:56-66`): the position and the
    /// owning chunk identifier. The port reports the set's `baseIndex_`
    /// in the chunk identifier's place (registered, as in
    /// `ChunkWaypointState::desc`).
    pub fn desc(&self) -> String {
        let set = match &self.p_set {
            Some(set) => set,
            None => return "(no set)".to_owned(),
        };
        format!(
            "({}, {}, {}) at {}",
            self.position.x,
            self.position.y,
            self.position.z,
            set.base_index()
        )
    }
}

impl AStarState for ChunkWPSetState {
    type SearchData = ();
    type Key = usize;

    /// `hash` (`chunk_waypoint_set_state.hpp:34-35`): the set identity.
    fn search_key(&self) -> usize {
        self.p_set
            .as_ref()
            .map_or(0, |set| Rc::as_ptr(set) as usize)
    }

    /// `compare` (`chunk_waypoint_set_state.hpp:29-31`): the raw pointer
    /// difference; only `== 0` is ever consumed.
    fn compare(&self, other: &Self) -> i64 {
        match (&self.p_set, &other.p_set) {
            (Some(a), Some(b)) => Rc::as_ptr(a) as usize as i64 - Rc::as_ptr(b) as usize as i64,
            (None, None) => 0,
            _ => 1,
        }
    }

    /// `isGoal` (`chunk_waypoint_set_state.hpp:37-38`): the set identity.
    fn is_goal(&self, goal: &Self) -> bool {
        match (&self.p_set, &goal.p_set) {
            (Some(a), Some(b)) => Rc::ptr_eq(a, b),
            (None, None) => true,
            _ => false,
        }
    }

    /// `distanceToGoal` (`chunk_waypoint_set_state.hpp:52-53`).
    fn distance_to_goal(&self, goal: &Self) -> f32 {
        let d = self.position - goal.position;
        (d.x * d.x + d.y * d.y + d.z * d.z).sqrt()
    }

    /// `distanceFromParent()` (`chunk_waypoint_set_state.hpp:49-50`).
    fn distance_from_parent(&self) -> f32 {
        self.distance_from_parent
    }

    /// `adjacenciesBegin`/`adjacenciesEnd`
    /// (`chunk_waypoint_set_state.hpp:40-44`): the connections.
    fn adjacency_count(&self) -> usize {
        self.p_set.as_ref().map_or(0, |set| set.connections().len())
    }

    /// `getAdjacency` (`chunk_waypoint_set_state.cpp:83-213`).
    fn get_adjacency(
        &self,
        index: usize,
        neigh: &mut Self,
        goal: &Self,
        _search_data: &mut Self::SearchData,
    ) -> bool {
        let set = match &self.p_set {
            Some(set) => set,
            None => return false,
        };
        let connections = set.connections();
        let (dest_set, portal) = match connections.get(index) {
            Some(entry) => entry,
            None => return false,
        };

        // if blocking non permissive, and this non-permissive, then don't
        // consider.
        if !portal.permissive && self.block_non_permissive {
            return false;
        }

        // Adjacent state inherits our value for blockNonPermissive_.
        neigh.block_non_permissive = self.block_non_permissive;

        // The reference's partner-portal scan runs here; the reciprocal
        // connection below subsumes it (a boundary scan miss implies no
        // reciprocal connection either) and its portal object is exactly
        // what the reciprocal connection stores.
        let back_portal = dest_set.connection_portal(set);
        if back_portal.is_none() {
            // The reference WARNINGs "one way portal" and refuses here.
            return false;
        }

        // Check that there is a waypoint to get back to set.
        if !dest_set
            .connections()
            .iter()
            .any(|(other, _)| Rc::ptr_eq(other, set))
        {
            return false;
        }

        // now check if portal activated or not.
        let from_chunk = set.chunk_identity();
        let to_chunk = dest_set.chunk_identity();
        neigh.passed_shell_boundary =
            from_chunk != to_chunk && (!set.is_outside_chunk() || !dest_set.is_outside_chunk());

        if neigh.passed_shell_boundary {
            neigh.passed_activated_portal =
                portal.has_chunk_item || back_portal.as_ref().is_some_and(|p| p.has_chunk_item);
        } else {
            neigh.passed_activated_portal = false;
        }

        neigh.p_set = Some(Rc::clone(dest_set));

        if neigh
            .p_set
            .as_ref()
            .and_then(|set| set.chunk_identity())
            .is_none()
        {
            // WARNING "Chunk associated with neighbouring waypoint set no
            // longer exists." in the reference.
            return false;
        }

        // If we are in the same set as the goal then use it as the
        // position, otherwise the closest edge label's entry point.
        if goal
            .p_set
            .as_ref()
            .is_some_and(|goal_set| Rc::ptr_eq(goal_set, dest_set))
        {
            neigh.position = goal.position;
        } else {
            let mut min_dist = f32::MAX;
            let mut closest_edge = None;
            for (_, label) in dest_set.edge_labels() {
                let d = label.entry_point - self.position;
                let dist = (d.x * d.x + d.y * d.y + d.z * d.z).sqrt();
                if dist < min_dist {
                    min_dist = dist;
                    closest_edge = Some(label.entry_point);
                }
            }
            match closest_edge {
                Some(point) => neigh.position = point,
                None => return false,
            }
        }

        neigh.distance_from_parent = self.distance_to_goal(neigh);

        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chunk_waypoint_set::Portal;
    use crate::chunk_waypoint_set_data::parse_world_navmesh;

    /// One encoded set record: a single waypoint square over the given
    /// edges (the parse assigns the base index).
    fn single_waypoint_set(edges: &[(f32, f32, i32)]) -> Rc<ChunkWaypointSetData> {
        let mut b = Vec::new();
        let put_f = |b: &mut Vec<u8>, v: f32| b.extend_from_slice(&v.to_le_bytes());
        let put_i = |b: &mut Vec<u8>, v: i32| b.extend_from_slice(&v.to_le_bytes());
        put_i(&mut b, 0);
        put_f(&mut b, 0.6);
        put_i(&mut b, 1);
        put_i(&mut b, edges.len() as i32);
        put_f(&mut b, 0.2);
        put_f(&mut b, 0.6);
        put_i(&mut b, edges.len() as i32);
        for (x, y, n) in edges {
            put_f(&mut b, *x);
            put_f(&mut b, *y);
            put_i(&mut b, *n);
        }
        let mut sets = parse_world_navmesh(&b).expect("decode");
        Rc::new(sets.remove(0))
    }

    const SQUARE: &[(f32, f32, i32)] = &[
        (0.0, 0.0, 65535),
        (0.0, 1.0, 65535),
        (1.0, 1.0, 65535),
        (1.0, 0.0, 65535),
    ];

    fn navloc(set: &Rc<ChunkWaypointSetData>, x: f32, y: f32, z: f32) -> NavLoc {
        NavLoc::from_parts(Rc::clone(set), 0, Vector3::new(x, y, z))
    }

    /// The A|B world: two mounted squares, A linked to B through
    /// `portal_ab` and B back through a permissive portal; B carries
    /// labels on edges 2 and 6.
    fn ab_world(
        permissive_ab: bool,
        has_item_ab: bool,
    ) -> (
        Rc<ChunkWaypointSetData>,
        Rc<ChunkWaypointSetData>,
        Rc<Portal>,
    ) {
        let a = single_waypoint_set(SQUARE);
        let b = single_waypoint_set(&[
            (1.0, 0.0, 65535),
            (1.0, 1.0, 65535),
            (2.0, 1.0, 65535),
            (2.0, 0.0, 65535),
        ]);
        a.set_chunk_identity(0);
        b.set_chunk_identity(1);

        let portal_ab = Rc::new(Portal {
            permissive: permissive_ab,
            has_chunk_item: has_item_ab,
        });
        let portal_ba = Rc::new(Portal {
            permissive: true,
            has_chunk_item: false,
        });
        a.connect(
            Rc::clone(&b),
            Rc::clone(&portal_ab),
            2,
            0,
            Vector3::new(1.0, 0.5, 0.5),
        );
        b.connect(Rc::clone(&a), portal_ba, 2, 0, Vector3::new(1.0, 0.5, 0.5));
        b.connect(
            Rc::clone(&a),
            Rc::new(Portal {
                permissive: true,
                has_chunk_item: false,
            }),
            6,
            0,
            Vector3::new(2.0, 0.5, 1.5),
        );
        (a, b, portal_ab)
    }

    #[test]
    fn the_state_constructors_and_comparisons_follow_the_reference() {
        let (a, b, _) = ab_world(true, false);

        // The NavLoc constructor keeps the set and the point.
        let state = ChunkWPSetState::from_navloc(&navloc(&a, 0.5, 0.41, 0.5));
        assert!(Rc::ptr_eq(state.p_set().unwrap(), &a));
        assert_eq!(state.position(), Vector3::new(0.5, 0.41, 0.5));
        assert!(!state.passed_shell_boundary());
        assert!(!state.passed_activated_portal());
        assert_eq!(state.distance_from_parent(), 0.0);
        assert_eq!(
            state.desc(),
            format!("(0.5, 0.41, 0.5) at {}", a.base_index())
        );

        // The default state has no set.
        let empty = ChunkWPSetState::default();
        assert!(empty.p_set().is_none());
        assert_eq!(empty.desc(), "(no set)");
        assert_eq!(empty.adjacency_count(), 0);

        // compare: the pointer difference; only == 0 is consumed.
        assert_eq!(state.compare(&state), 0);
        let on_b = ChunkWPSetState::from_navloc(&navloc(&b, 0.0, 0.0, 0.0));
        assert_ne!(state.compare(&on_b), 0);
        assert_eq!(empty.compare(&empty), 0);

        // isGoal: the set identity (two nulls agree).
        assert!(state.is_goal(&state));
        assert!(!state.is_goal(&on_b));
        assert!(empty.is_goal(&ChunkWPSetState::default()));

        // hash: the set address.
        assert_eq!(state.search_key(), Rc::as_ptr(&a) as usize);
        assert_eq!(empty.search_key(), 0);

        // distanceToGoal: the plain length.
        let goal = ChunkWPSetState::from_navloc(&navloc(&b, 1.5, 0.41, 1.0));
        let expected = (1.0f32 * 1.0 + 0.5 * 0.5).sqrt();
        assert!((state.distance_to_goal(&goal) - expected).abs() <= 1e-5);

        // An index past the connections answers false, as does a state
        // without a set.
        let mut neigh = ChunkWPSetState::default();
        let mut data = ();
        assert!(!state.get_adjacency(9, &mut neigh, &goal, &mut data));
        assert!(!empty.get_adjacency(0, &mut neigh, &goal, &mut data));
    }

    #[test]
    fn the_adjacency_walks_the_connection_and_picks_the_nearest_label() {
        let (a, b, portal_ab) = ab_world(true, false);
        let state = ChunkWPSetState::from_navloc(&navloc(&a, 0.5, 0.41, 0.5));
        assert_eq!(state.adjacency_count(), 1);

        // The goal sits on a set other than the destination: the position
        // comes from the label scan (a goal on the destination would take
        // the goal position straight away).
        let goal = ChunkWPSetState::from_navloc(&navloc(&a, 2.5, 0.41, 0.5));
        let mut neigh = ChunkWPSetState::default();
        let mut data = ();
        assert!(state.get_adjacency(0, &mut neigh, &goal, &mut data));

        // The destination set and the inherited block flag.
        assert!(Rc::ptr_eq(neigh.p_set().unwrap(), &b));
        assert!(!neigh.passed_activated_portal());

        // Shell boundary: different chunks, neither outside.
        assert!(neigh.passed_shell_boundary());

        // Position: the label entry nearest the current state's position
        // (edge 2's at (1, 0.5, 0.5) beats edge 6's at (2, 0.5, 1.5)).
        assert_eq!(neigh.position(), Vector3::new(1.0, 0.5, 0.5));

        // distanceFromParent = this->distanceToGoal(neigh).
        let expected = (0.5f32 * 0.5 + 0.09 * 0.09).sqrt();
        assert!((neigh.distance_from_parent() - expected).abs() <= 1e-5);

        // A goal on the destination set keeps the goal position instead
        // of a label entry.
        let goal_on_b = ChunkWPSetState::from_navloc(&navloc(&b, 2.0, 0.41, 0.9));
        let mut neigh = ChunkWPSetState::default();
        assert!(state.get_adjacency(0, &mut neigh, &goal_on_b, &mut data));
        assert_eq!(neigh.position(), goal_on_b.position());

        // The portal identity is the one the connection stored.
        assert!(Rc::ptr_eq(&a.connection_portal(&b).unwrap(), &portal_ab));
    }

    #[test]
    fn the_non_permissive_gate_only_blocks_when_set() {
        let (a, b, _) = ab_world(false, false);
        let goal = ChunkWPSetState::from_navloc(&navloc(&b, 2.5, 0.41, 0.5));
        let mut neigh = ChunkWPSetState::default();
        let mut data = ();

        // Without the block the non-permissive connection still walks.
        let state = ChunkWPSetState::from_navloc(&navloc(&a, 0.5, 0.41, 0.5));
        assert!(state.get_adjacency(0, &mut neigh, &goal, &mut data));

        // With it, the adjacency refuses.
        let mut blocking = ChunkWPSetState::from_navloc(&navloc(&a, 0.5, 0.41, 0.5));
        blocking.set_block_non_permissive(true);
        assert!(!blocking.get_adjacency(0, &mut neigh, &goal, &mut data));
    }

    #[test]
    fn the_activated_flag_reads_the_portal_items_inside_the_shell() {
        let goal_of =
            |b: &Rc<ChunkWaypointSetData>| ChunkWPSetState::from_navloc(&navloc(b, 2.5, 0.41, 0.5));

        // A chunk item on the outbound portal marks the crossing
        // activated inside the shell.
        let (a, b, _) = ab_world(true, true);
        let state = ChunkWPSetState::from_navloc(&navloc(&a, 0.5, 0.41, 0.5));
        let mut neigh = ChunkWPSetState::default();
        let mut data = ();
        assert!(state.get_adjacency(0, &mut neigh, &goal_of(&b), &mut data));
        assert!(neigh.passed_shell_boundary());
        assert!(neigh.passed_activated_portal());

        // The same chunk on both sides: no shell boundary, no activated
        // flag even with the item set.
        let (a, b, _) = ab_world(true, true);
        b.set_chunk_identity(0);
        let state = ChunkWPSetState::from_navloc(&navloc(&a, 0.5, 0.41, 0.5));
        let mut neigh = ChunkWPSetState::default();
        assert!(state.get_adjacency(0, &mut neigh, &goal_of(&b), &mut data));
        assert!(!neigh.passed_shell_boundary());
        assert!(!neigh.passed_activated_portal());

        // Two outside chunks with different ids fail the shell formula
        // (both flags outside).
        let (a, b, _) = ab_world(true, false);
        a.set_outside_chunk(true);
        b.set_outside_chunk(true);
        let state = ChunkWPSetState::from_navloc(&navloc(&a, 0.5, 0.41, 0.5));
        let mut neigh = ChunkWPSetState::default();
        assert!(state.get_adjacency(0, &mut neigh, &goal_of(&b), &mut data));
        assert!(!neigh.passed_shell_boundary());
        assert!(!neigh.passed_activated_portal());
    }

    #[test]
    fn the_reciprocal_gate_refuses_a_one_way_connection() {
        let a = single_waypoint_set(SQUARE);
        a.set_chunk_identity(0);
        let lone = single_waypoint_set(SQUARE);
        lone.set_chunk_identity(2);
        let portal = Rc::new(Portal {
            permissive: true,
            has_chunk_item: false,
        });
        // A connects out; `lone` never connects back.
        a.connect(Rc::clone(&lone), portal, 2, 0, Vector3::new(1.0, 0.5, 0.5));

        let state = ChunkWPSetState::from_navloc(&navloc(&a, 0.5, 0.41, 0.5));
        let goal = ChunkWPSetState::from_navloc(&navloc(&lone, 1.5, 0.41, 0.5));
        let mut neigh = ChunkWPSetState::default();
        let mut data = ();
        assert!(!state.get_adjacency(0, &mut neigh, &goal, &mut data));
    }

    #[test]
    fn the_unmounted_destination_refuses() {
        let (a, _b, _) = ab_world(true, false);
        // A second destination without a chunk identity (the chunk-gone
        // WARNING arm), linked reciprocally.
        let unmounted = single_waypoint_set(SQUARE);
        let portal_u = Rc::new(Portal {
            permissive: true,
            has_chunk_item: false,
        });
        a.connect(
            Rc::clone(&unmounted),
            Rc::clone(&portal_u),
            3,
            0,
            Vector3::new(1.0, 0.5, 0.8),
        );
        unmounted.connect(Rc::clone(&a), portal_u, 2, 0, Vector3::new(1.0, 0.5, 0.5));

        let state = ChunkWPSetState::from_navloc(&navloc(&a, 0.5, 0.41, 0.5));
        let goal = ChunkWPSetState::from_navloc(&navloc(&unmounted, 1.5, 0.41, 0.5));
        let mut neigh = ChunkWPSetState::default();
        let mut data = ();
        // Adjacency 0 is the mounted B; adjacency 1 the unmounted set.
        assert!(state.get_adjacency(0, &mut neigh, &goal, &mut data));
        assert!(!state.get_adjacency(1, &mut neigh, &goal, &mut data));
    }

    #[test]
    fn a_labelless_destination_refuses() {
        let (a, b, _) = ab_world(true, false);
        // The empty-edgeLabels arm is defensive in the reference too (a
        // real connect always writes its label); clear the crate-private
        // map to reach it.
        b.edge_labels.borrow_mut().clear();

        let state = ChunkWPSetState::from_navloc(&navloc(&a, 0.5, 0.41, 0.5));
        // The goal sits on A: a goal on the destination would skip the
        // label scan and answer true regardless.
        let goal = ChunkWPSetState::from_navloc(&navloc(&a, 2.5, 0.41, 0.5));
        let mut neigh = ChunkWPSetState::default();
        let mut data = ();
        assert!(!state.get_adjacency(0, &mut neigh, &goal, &mut data));
    }
}
