//! `lib/waypoint/chunk_waypoint_state.{hpp,cpp}` — the A* state over the
//! waypoint graph and the per-search edge-connection bookkeeping.
//!
//! The reference reaches `NavLoc`'s private fields through its
//! `friend class ChunkWaypointState` clause; the port builds and mutates
//! locations through the crate-internal `NavLoc::from_parts` /
//! `clip_in_place` pair instead.

use super::chunk_waypoint::ChunkWaypointVertexProvider;
use super::chunk_waypoint_set_data::ChunkWaypointSetData;
use super::navloc::NavLoc;
use super::{EdgeIndex, OTHER_CHUNK, WaypointIndex};
use foundation::math::{Vector2, Vector3};
use std::collections::HashMap;
use std::rc::Rc;

/// `ChunkWaypointState` (`chunk_waypoint_state.hpp:26-84`): one state in an
/// A* search of the waypoint graph.
#[derive(Debug, Clone)]
pub struct ChunkWaypointState {
    /// `navLoc_`.
    nav_loc: NavLoc,
    /// `distanceFromParent_`.
    distance_from_parent: f32,
    /// `crossedPortal_`.
    crossed_portal: bool,
}

impl Default for ChunkWaypointState {
    fn default() -> Self {
        Self {
            nav_loc: NavLoc::new(),
            distance_from_parent: 0.0,
            crossed_portal: false,
        }
    }
}

impl ChunkWaypointState {
    /// `ChunkWaypointState( ChunkWaypointSetPtr dstSet, const Vector3 &
    /// dstPoint )` (`chunk_waypoint_state.cpp:29-43`): a goal-only state on
    /// the destination set — waypoint -1, the point lifted by `clip`.
    pub fn for_destination(set: Rc<ChunkWaypointSetData>, dst_point: Vector3) -> Self {
        let mut nav_loc = NavLoc::from_parts(set, -1, dst_point);
        nav_loc.clip_in_place();
        Self {
            nav_loc,
            distance_from_parent: 0.0,
            crossed_portal: false,
        }
    }

    /// `ChunkWaypointState( const NavLoc & navLoc )`
    /// (`chunk_waypoint_state.cpp:45-51`).
    pub fn from_navloc(nav_loc: &NavLoc) -> Self {
        Self {
            nav_loc: nav_loc.clone(),
            distance_from_parent: 0.0,
            crossed_portal: false,
        }
    }

    /// `desc` (`chunk_waypoint_state.cpp:53-70`): the waypoint, the point,
    /// the owning chunk identifier and the portal flag. The reference
    /// prefixes `no set` / `no chunk`; the port answers `no set` for a
    /// stateless location and reports the set's `baseIndex_` in the chunk
    /// identifier's place, since the chunk mounting is out of this face
    /// (registered).
    pub fn desc(&self) -> String {
        let set = match self.nav_loc.p_set() {
            Some(set) => set,
            None => return "no set".to_owned(),
        };
        let point = self.nav_loc.point();
        let mut out = format!(
            "{} ({}, {}, {})",
            self.nav_loc.waypoint(),
            point.x,
            point.y,
            point.z
        );
        out.push_str(&format!(" in {}", set.base_index()));
        if self.crossed_portal {
            out.push_str(" (portal)");
        }
        out
    }

    /// `compare` (`chunk_waypoint_state.hpp:37-45`): the same set compares
    /// waypoints; different sets compare raw pointers. Only `== 0` is ever
    /// consumed.
    pub fn compare(&self, other: &ChunkWaypointState) -> i64 {
        match (self.nav_loc.p_set(), other.nav_loc.p_set()) {
            (Some(a), Some(b)) => {
                if Rc::ptr_eq(a, b) {
                    i64::from(self.nav_loc.waypoint() - other.nav_loc.waypoint())
                } else {
                    Rc::as_ptr(a) as usize as i64 - Rc::as_ptr(b) as usize as i64
                }
            }
            _ => 0,
        }
    }

    /// `isGoal` (`chunk_waypoint_state.hpp:48-56`): a goal state with
    /// waypoint -1 answers on the set identity, otherwise on the same
    /// set-and-waypoint pair.
    pub fn is_goal(&self, goal: &ChunkWaypointState) -> bool {
        let goal_loc = goal.nav_loc();
        if goal_loc.waypoint() == -1 {
            return match (self.nav_loc.p_set(), goal_loc.p_set()) {
                (Some(a), Some(b)) => Rc::ptr_eq(a, b),
                _ => false,
            };
        }
        self.nav_loc.waypoints_equal(goal_loc)
    }

    /// `distanceToGoal` (`chunk_waypoint_state.hpp:79-81`): the plain 3D
    /// length between the two states' points.
    pub fn distance_to_goal(&self, goal: &ChunkWaypointState) -> f32 {
        let d = self.nav_loc().point() - goal.nav_loc().point();
        (d.x * d.x + d.y * d.y + d.z * d.z).sqrt()
    }

    /// `navLoc()` (`chunk_waypoint_state.hpp:83-84`).
    pub fn nav_loc(&self) -> &NavLoc {
        &self.nav_loc
    }

    /// `distanceFromParent()` (`chunk_waypoint_state.hpp:71-72`).
    pub fn distance_from_parent(&self) -> f32 {
        self.distance_from_parent
    }

    /// The adjacency count (`adjacenciesEnd - adjacenciesBegin`,
    /// `chunk_waypoint_state.hpp:58-69`): the located waypoint's edge count,
    /// or zero when the state does not sit on a waypoint.
    pub(crate) fn adjacency_count(&self) -> usize {
        let waypoint = self.nav_loc.waypoint();
        if waypoint >= 0
            && let Some(set) = self.nav_loc.p_set()
        {
            let wp = &set.waypoints()[waypoint as usize];
            return wp.edge_count as usize;
        }
        0
    }

    /// `getAdjacency` (`chunk_waypoint_state.cpp:53-190`): the neighbour
    /// over edge `index`, its connection point computed where the straight
    /// line to the goal crosses the shared edge (or the closer of the
    /// edge's two vertices).
    ///
    /// Registered boundary: the cross-set branch (`waypointAdjToChunk`,
    /// resolved through `edgeLabel`) answers false until the connections
    /// and portal data land in the cross-chunk face (BW-10-04-05).
    pub fn get_adjacency(
        &self,
        index: usize,
        neigh: &mut ChunkWaypointState,
        goal: &ChunkWaypointState,
        search_data: &mut ChunkWaypointStateSearchData,
    ) -> bool {
        let set = match self.nav_loc.p_set() {
            Some(set) => set,
            None => return false,
        };
        let waypoint = self.nav_loc.waypoint();
        if waypoint < 0 {
            return false;
        }
        let cw = &set.waypoints()[waypoint as usize];
        let edge_range = cw.edge_range();
        let cwe = set.edges()[edge_range.start + index];
        let waypoint = cwe.neighbouring_waypoint();
        let waypoint_adj_to_chunk = cwe.adjacent_to_chunk();

        if self.crossed_portal {
            return false;
        }

        if waypoint >= 0 {
            // In-set neighbour: rebuild the neighbour location in place.
            let set2 = Rc::clone(set);
            neigh.nav_loc = NavLoc::from_parts(set2, waypoint, Vector3::default());
            neigh.crossed_portal = false;
        } else if waypoint_adj_to_chunk {
            // `edgeLabel` carries the neighbouring chunk's set; the port has
            // no edge labels yet, so the reference's null-label early-out is
            // what always answers here.
            return false;
        } else {
            return false;
        }

        let src = Vector2::new(self.nav_loc.point().x, self.nav_loc.point().z);
        let dst = Vector2::new(goal.nav_loc().point().x, goal.nav_loc().point().z);
        let mut way;
        let del = Vector2::new(dst.x - src.x, dst.y - src.y);
        let vp1 = cwe.vertex_index;
        let vp2 = set.edges()[edge_range.start + (index + 1) % cw.edge_count as usize].vertex_index;
        let p1 = *set.vertex_by_index(vp1);
        let p2 = *set.vertex_by_index(vp2);

        let cp1 = del.x * (p1.y - src.y) - del.y * (p1.x - src.x);
        let cp2 = del.x * (p2.y - src.y) - del.y * (p2.x - src.x);
        // see if our path goes through this edge
        if cp1 > 0.0 && cp2 < 0.0 {
            // calculate the intersection of the line (src->dst) and
            // (p1->p2): cp1 and cp2 are the parallelogram areas, so the
            // ratio is the perpendicular-height ratio of p1 and p2 to del.
            let t = cp1 / (cp1 - cp2);
            way = Vector2::new(p1.x + t * (p2.x - p1.x), p1.y + t * (p2.y - p1.y));
        } else {
            let len = |a: Vector2, b: Vector2| -> f32 {
                ((a.x - b.x) * (a.x - b.x) + (a.y - b.y) * (a.y - b.y)).sqrt()
            };
            let d1 = len(p1, dst) + len(p1, src);
            let d2 = len(p2, dst) + len(p2, src);
            way = if d1 < d2 { p1 } else { p2 };
        }

        if neigh.nav_loc.waypoint() == OTHER_CHUNK {
            nudge_past_boundary(&mut way, del);
        }

        // `wway( way.x, cw.maxHeight_, way.y )` through the identity
        // mapping (registered).
        neigh.nav_loc = NavLoc::from_parts(
            Rc::clone(set),
            neigh.nav_loc.waypoint(),
            Vector3::new(way.x, cw.max_height, way.y),
        );

        search_data.set_connection(
            self.nav_loc.p_set(),
            self.nav_loc.waypoint(),
            neigh.nav_loc.p_set(),
            neigh.nav_loc.waypoint(),
            vp1,
            vp2,
        );

        // The chunk-box clamp of `waypointAdjToChunk` needs the chunk face;
        // registered with the branch above (BW-10-04-05).

        neigh.nav_loc.clip_in_place();
        let np = neigh.nav_loc.point();
        let sp = self.nav_loc.point();
        neigh.distance_from_parent = ((np.x - sp.x) * (np.x - sp.x)
            + (np.y - sp.y) * (np.y - sp.y)
            + (np.z - sp.z) * (np.z - sp.z))
            .sqrt();

        true
    }
}

/// The goal-only neighbour's boundary step (`neigh.navLoc_.waypoint_ ==
/// -1` branch of `getAdjacency`): `del.normalise(); way += del * 0.2f`.
/// The port only reaches it through edge labels landing on waypoint -1
/// (the cross-chunk face, BW-10-04-05), so it is factored out for direct
/// testing.
fn nudge_past_boundary(way: &mut Vector2, del: Vector2) {
    let len = (del.x * del.x + del.y * del.y).sqrt();
    let (nx, ny) = if len > 0.0 {
        (del.x / len, del.y / len)
    } else {
        (del.x, del.y)
    };
    way.x += nx * 0.2;
    way.y += ny * 0.2;
}

/// The connection map's shapes: the ordered from/to (set pointer,
/// waypoint) pair keyed against the edge's two vertex indices.
type ConnectionKey = ((usize, WaypointIndex), (usize, WaypointIndex));
type ConnectionPair = (EdgeIndex, EdgeIndex);

/// `ChunkWaypointStateSearchData` (`chunk_waypoint_state.hpp:89-115`): the
/// edge-vertex pairs recorded while expanding, keyed by the ordered
/// from/to waypoint pair.
///
/// Registered boundary: the reference hashes raw `ChunkWaypointSet*`
/// pointers; the port keys the `Rc` address — same identity, different
/// allocation.
#[derive(Debug, Default)]
pub struct ChunkWaypointStateSearchData {
    connections: HashMap<ConnectionKey, ConnectionPair>,
}

impl ChunkWaypointStateSearchData {
    /// `setConnection` (`chunk_waypoint_state.cpp:194-210`).
    fn set_connection(
        &mut self,
        from_set: Option<&Rc<ChunkWaypointSetData>>,
        from: WaypointIndex,
        to_set: Option<&Rc<ChunkWaypointSetData>>,
        to: WaypointIndex,
        vp1: EdgeIndex,
        vp2: EdgeIndex,
    ) {
        let key_from = (from_set.map(|s| Rc::as_ptr(s) as usize).unwrap_or(0), from);
        let key_to = (to_set.map(|s| Rc::as_ptr(s) as usize).unwrap_or(0), to);
        self.connections.insert((key_from, key_to), (vp1, vp2));
    }

    /// `getConnection` (`chunk_waypoint_state.cpp:212-235`): the recorded
    /// pair, or the `adjacentRangeEnd` sentinel twice.
    pub fn get_connection(
        &self,
        from_set: &Rc<ChunkWaypointSetData>,
        from: WaypointIndex,
        to_set: &Rc<ChunkWaypointSetData>,
        to: WaypointIndex,
    ) -> (EdgeIndex, EdgeIndex) {
        let key_from = (Rc::as_ptr(from_set) as usize, from);
        let key_to = (Rc::as_ptr(to_set) as usize, to);
        match self.connections.get(&(key_from, key_to)) {
            Some(pair) => *pair,
            None => (
                super::chunk_waypoint::ADJACENT_RANGE_END,
                super::chunk_waypoint::ADJACENT_RANGE_END,
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chunk_waypoint_set_data::parse_world_navmesh;

    // A two-waypoint square split along x = 1: waypoint 0 is x in [0, 1],
    // waypoint 1 is x in [1, 2], sharing the edge (1,0)-(1,2). CW winding.
    fn split_square() -> Rc<ChunkWaypointSetData> {
        let mut b = Vec::new();
        let put_f = |b: &mut Vec<u8>, v: f32| b.extend_from_slice(&v.to_le_bytes());
        let put_i = |b: &mut Vec<u8>, v: i32| b.extend_from_slice(&v.to_le_bytes());
        let put_e = |b: &mut Vec<u8>, x: f32, y: f32, n: i32| {
            put_f(b, x);
            put_f(b, y);
            put_i(b, n);
        };

        put_i(&mut b, 0); // version
        put_f(&mut b, 0.6); // girth
        put_i(&mut b, 2); // numWaypoints
        put_i(&mut b, 8); // numEdges
        // The waypoint header block lands first, then the edge stream
        // (the decode's two cursors).
        // wp0: x in [0,1], heights 0..1. Edge 1 — the shared x=1 edge —
        // borders wp1.
        put_f(&mut b, 0.0);
        put_f(&mut b, 1.0);
        put_i(&mut b, 4);
        // wp1: x in [1,2]. Edge 3 borders wp0.
        put_f(&mut b, 0.0);
        put_f(&mut b, 1.0);
        put_i(&mut b, 4);
        put_e(&mut b, 0.0, 2.0, 65535);
        put_e(&mut b, 1.0, 2.0, 1);
        put_e(&mut b, 1.0, 0.0, 65535);
        put_e(&mut b, 0.0, 0.0, 65535);
        put_e(&mut b, 1.0, 2.0, 65535);
        put_e(&mut b, 2.0, 2.0, 65535);
        put_e(&mut b, 2.0, 0.0, 65535);
        put_e(&mut b, 1.0, 0.0, 0);

        let mut sets = parse_world_navmesh(&b).expect("decode");
        Rc::new(sets.remove(0))
    }

    fn state_on(
        set: &Rc<ChunkWaypointSetData>,
        waypoint: WaypointIndex,
        x: f32,
        z: f32,
    ) -> ChunkWaypointState {
        ChunkWaypointState::from_navloc(&NavLoc::from_parts(
            Rc::clone(set),
            waypoint,
            Vector3::new(x, 0.5, z),
        ))
    }

    #[test]
    fn the_goal_state_clips_and_stays_waypoint_negative() {
        let set = split_square();
        let goal =
            ChunkWaypointState::for_destination(Rc::clone(&set), Vector3::new(1.5, 0.5, 1.0));
        assert_eq!(goal.nav_loc().waypoint(), -1);
        assert_eq!(goal.nav_loc().point(), Vector3::new(1.5, 0.5, 1.0));
    }

    #[test]
    fn compare_orders_by_waypoint_within_a_set() {
        let set = split_square();
        let a = state_on(&set, 0, 0.5, 0.5);
        let b = state_on(&set, 1, 1.5, 0.5);
        assert_eq!(a.compare(&a), 0);
        assert_eq!(a.compare(&b), -1);
        assert_eq!(b.compare(&a), 1);
    }

    #[test]
    fn is_goal_answers_on_the_waypoint_pair() {
        let set = split_square();
        let a = state_on(&set, 0, 0.5, 0.5);
        let b = state_on(&set, 1, 1.5, 0.5);
        assert!(!a.is_goal(&b));
        assert!(a.is_goal(&a));

        // A waypoint -1 goal answers on the set identity.
        let goal =
            ChunkWaypointState::for_destination(Rc::clone(&set), Vector3::new(1.5, 0.5, 1.0));
        assert!(a.is_goal(&goal));
        assert!(b.is_goal(&goal));
    }

    #[test]
    fn the_shared_edge_crosses_where_the_line_meets_it() {
        let set = split_square();
        let src = state_on(&set, 0, 0.5, 1.0);
        let goal = state_on(&set, 1, 1.5, 1.5);
        let mut neigh = ChunkWaypointState::default();
        let mut search_data = ChunkWaypointStateSearchData::default();

        // Edge 1 of wp0 is (1,0)->(1,2); the line (0.5,1.0)->(1.5,1.5)
        // crosses it at z = 1.25.
        assert!(src.get_adjacency(1, &mut neigh, &goal, &mut search_data));
        assert_eq!(neigh.nav_loc().waypoint(), 1);
        let p = neigh.nav_loc().point();
        assert!((p.x - 1.0).abs() <= 1e-5);
        assert!((p.z - 1.25).abs() <= 1e-5);
        assert_eq!(p.y, 1.0, "the crossing point rides the source maxHeight");
        assert_eq!(
            neigh.distance_from_parent(),
            (0.5f32 * 0.5 + 0.5 * 0.5 + 0.25 * 0.25).sqrt()
        );

        // The recorded connection carries the edge's two vertex indices.
        assert_eq!(
            search_data.get_connection(
                src.nav_loc().p_set().unwrap(),
                0,
                neigh.nav_loc().p_set().unwrap(),
                1
            ),
            (1, 2),
            "edge 1 starts at vertex (1,0), the next edge at (1,2)"
        );
    }

    #[test]
    fn a_line_missing_the_shared_edge_takes_the_closer_vertex() {
        let set = split_square();
        // Goal outside the strip: the line from (0.5, 0.2) to (1.5, -3)
        // runs past the shared edge's lower end, so the connection point
        // snaps to the closer endpoint (1, 0).
        let src = state_on(&set, 0, 0.5, 0.2);
        let goal = state_on(&set, 1, 1.5, -3.0);
        let mut neigh = ChunkWaypointState::default();
        let mut search_data = ChunkWaypointStateSearchData::default();
        assert!(src.get_adjacency(1, &mut neigh, &goal, &mut search_data));
        let p = neigh.nav_loc().point();
        assert!((p.x - 1.0).abs() <= 1e-5);
        assert!((p.z - 0.0).abs() <= 1e-5);
    }

    #[test]
    fn the_boundary_nudge_steps_along_the_normalised_line() {
        // way += del.normalise() * 0.2
        let mut way = Vector2::new(1.0, 1.0);
        nudge_past_boundary(&mut way, Vector2::new(3.0, 4.0));
        assert!((way.x - (1.0 + 0.6 * 0.2)).abs() <= 1e-5);
        assert!((way.y - (1.0 + 0.8 * 0.2)).abs() <= 1e-5);

        // A zero-length del leaves the direction untouched.
        let mut way = Vector2::new(1.0, 1.0);
        nudge_past_boundary(&mut way, Vector2::new(0.0, 0.0));
        assert_eq!(way, Vector2::new(1.0, 1.0));
    }

    #[test]
    fn a_waypoint_negative_goal_still_crosses_in_set_edges() {
        let set = split_square();
        let src = state_on(&set, 0, 0.5, 1.0);
        // The goal-only state on the same set answers on the set identity.
        let goal =
            ChunkWaypointState::for_destination(Rc::clone(&set), Vector3::new(3.0, 0.5, 1.0));
        let mut neigh = ChunkWaypointState::default();
        let mut search_data = ChunkWaypointStateSearchData::default();
        assert!(src.get_adjacency(1, &mut neigh, &goal, &mut search_data));
        assert_eq!(neigh.nav_loc().waypoint(), 1);
        assert!(src.is_goal(&goal));
    }

    #[test]
    fn chunk_adjacent_edges_and_bad_states_answer_false() {
        let set = split_square();
        let src = state_on(&set, 0, 0.5, 1.0);
        let goal = state_on(&set, 1, 1.5, 1.0);
        let mut neigh = ChunkWaypointState::default();
        let mut search_data = ChunkWaypointStateSearchData::default();

        // Edges 0/2/3 of wp0 are chunk-adjacent with no edge label.
        assert!(!src.get_adjacency(0, &mut neigh, &goal, &mut search_data));
        assert!(!src.get_adjacency(2, &mut neigh, &goal, &mut search_data));
        assert!(!src.get_adjacency(3, &mut neigh, &goal, &mut search_data));

        // A state without a set or off a waypoint has no adjacencies.
        let empty = ChunkWaypointState::default();
        assert!(!empty.get_adjacency(0, &mut neigh, &goal, &mut search_data));
        assert_eq!(empty.adjacency_count(), 0);

        assert_eq!(src.adjacency_count(), 4);
    }

    #[test]
    fn the_connection_lookup_misses_answer_the_sentinel() {
        let set = split_square();
        let search_data = ChunkWaypointStateSearchData::default();
        assert_eq!(
            search_data.get_connection(&set, 0, &set, 1),
            (
                super::super::chunk_waypoint::ADJACENT_RANGE_END,
                super::super::chunk_waypoint::ADJACENT_RANGE_END
            )
        );
    }

    #[test]
    fn desc_reports_the_waypoint_the_point_and_the_portal_flag() {
        let set = split_square();
        let src = state_on(&set, 0, 0.5, 1.0);
        assert_eq!(
            src.desc(),
            format!("0 (0.5, 0.5, 1) in {}", set.base_index())
        );

        // The portal suffix is the cross-chunk marker.
        let mut portal = src.clone();
        portal.crossed_portal = true;
        assert!(portal.desc().ends_with(" (portal)"), "{}", portal.desc());

        // The stateless location answers the reference's first guard.
        assert_eq!(ChunkWaypointState::default().desc(), "no set");
    }

    #[test]
    fn cross_set_and_setless_compares_follow_the_pointer_rule() {
        // Two decodes of the same record are different set objects: the
        // pointer difference decides (the reference compares raw pointers).
        let a_set = split_square();
        let b_set = split_square();
        let a = state_on(&a_set, 0, 0.5, 1.0);
        let b = state_on(&b_set, 0, 0.5, 1.0);
        assert_ne!(a.compare(&b), 0);

        // Either side without a set answers equal (the reference compares
        // null pointers here).
        let empty = ChunkWaypointState::default();
        assert_eq!(empty.compare(&empty), 0);
        assert_eq!(empty.compare(&a), 0);
        assert_eq!(a.compare(&empty), 0);

        // A waypoint -1 goal against a setless state is not the goal.
        let goal =
            ChunkWaypointState::for_destination(Rc::clone(&a_set), Vector3::new(1.5, 0.5, 1.0));
        assert!(!goal.is_goal(&empty));
    }

    #[test]
    fn a_goal_only_state_has_no_adjacencies() {
        let set = split_square();
        let goal =
            ChunkWaypointState::for_destination(Rc::clone(&set), Vector3::new(1.5, 0.5, 1.0));
        let other = state_on(&set, 1, 1.5, 1.0);
        let mut neigh = ChunkWaypointState::default();
        let mut search_data = ChunkWaypointStateSearchData::default();
        assert_eq!(goal.adjacency_count(), 0);
        assert!(!goal.get_adjacency(0, &mut neigh, &other, &mut search_data));
    }

    #[test]
    fn a_crossed_portal_state_refuses_further_adjacencies() {
        let set = split_square();
        let mut src = state_on(&set, 0, 0.5, 1.0);
        let goal = state_on(&set, 1, 1.5, 1.0);
        src.crossed_portal = true;
        let mut neigh = ChunkWaypointState::default();
        let mut search_data = ChunkWaypointStateSearchData::default();
        assert!(!src.get_adjacency(1, &mut neigh, &goal, &mut search_data));
    }
}
