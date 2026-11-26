//! `lib/waypoint/astar.{hpp,ipp}` — the generic A* over waypoint states,
//! instantiated for `ChunkWaypointState` (the waypoint graph) and
//! `ChunkWPSetState` (the portal connections).
//!
//! Registered boundaries (allocation faces, same answers):
//! - the reference `open_` is a `BW::set<IntState*>` iterated in pointer
//!   order for the min-f scan; the port keeps a `Vec` in insertion order.
//!   Among states that compare equal on `(f, g)` the winner is then
//!   insertion-ordered rather than address-ordered — the reference order is
//!   allocation-dependent and not part of the contract.
//! - the `IntStateHash` (257 bins over `ext.hash()`) becomes a `HashMap`
//!   keyed by the state's `search_key` — the same identity its `compare`
//!   checks.
//! - the `StateScopedFreeList` (alloc/free/placement-new reuse) becomes an
//!   arena `Vec`; each search state is destroyed exactly once there.

use super::girth::almost_equal;
use std::collections::HashMap;
use std::fmt::Debug;

/// The state contract the reference's `AStar` template reads off `State`
/// (`astar.hpp:36-56`): `Default` plays the freelist's placement-new,
/// `search_key` the hash, `compare` the pointer-equality loop guard.
pub trait AStarState: Clone + Default + Debug {
    /// `SearchData` — the per-search bookkeeping `getAdjacency` writes.
    type SearchData: Default;
    /// The close-set identity (`IntStateHash` over `ext.hash()`).
    type Key: Eq + std::hash::Hash;

    /// `hash()`.
    fn search_key(&self) -> Self::Key;
    /// `compare()` (only `== 0` is consumed).
    fn compare(&self, other: &Self) -> i64;
    /// `isGoal()`.
    fn is_goal(&self, goal: &Self) -> bool;
    /// `distanceToGoal()`.
    fn distance_to_goal(&self, goal: &Self) -> f32;
    /// `distanceFromParent()`.
    fn distance_from_parent(&self) -> f32;
    /// `adjacenciesEnd() - adjacenciesBegin()`.
    fn adjacency_count(&self) -> usize;
    /// `getAdjacency()`.
    fn get_adjacency(
        &self,
        index: usize,
        neigh: &mut Self,
        goal: &Self,
        search_data: &mut Self::SearchData,
    ) -> bool;
}

/// `cmp_f` (`astar.hpp:220-231`): the min-f scan's better-than test; f ties
/// within the almostEqual epsilon prefer the larger g.
fn cmp_f<S: AStarState>(a: &AStarNode<S>, b: &AStarNode<S>) -> bool {
    if almost_equal(a.f, b.f) {
        a.g > b.g
    } else {
        a.f < b.f
    }
}

/// One `IntState` (`astar.hpp:60-79`): the state extension plus the search
/// bookkeeping. The child link only exists for the found path (the
/// reference back-links it on the goal walk) and the reference's stored
/// `h` is never read back (`f` carries it); the port collects the path
/// through the parent chain, so neither is modelled.
#[derive(Debug, Default)]
struct AStarNode<S: AStarState> {
    ext: S,
    g: f32,
    f: f32,
    parent: Option<usize>,
}

/// `AStar<State, State::SearchData>` (`astar.hpp:36-56`): the search over
/// the state's adjacency graph.
#[derive(Debug, Default)]
pub struct AStar<S: AStarState> {
    /// The `StateScopedFreeList` arena.
    nodes: Vec<AStarNode<S>>,
    /// The open set — states not yet fully expanded.
    open: Vec<usize>,
    /// The close set — every state encountered so far.
    close: HashMap<S::Key, usize>,
    /// `start_`: the initial node, the path's first entry.
    start: Option<usize>,
    /// `searchData_`: the edge connections recorded while expanding.
    search_data: S::SearchData,
    /// The path in start-to-goal order once a search succeeded.
    path: Vec<S>,
}

impl<S: AStarState> AStar<S> {
    /// `AStar::search` (`astar.ipp:37-197`).
    pub fn search(&mut self, start: &S, goal: &S, max_distance: f32) -> bool {
        // "Ensure that recalling this doesn't recycle old data"
        self.nodes.clear();
        self.open.clear();
        self.close.clear();
        self.search_data = S::SearchData::default();
        self.path.clear();

        let check_max_dist = max_distance > 0.0;

        // Create an initial start node, and push it onto the open queue.
        self.nodes.push(AStarNode {
            ext: start.clone(),
            g: 0.0,
            f: start.distance_to_goal(goal),
            parent: None,
        });
        let start_index = 0;
        self.start = Some(start_index);
        self.open.push(start_index);

        let start_state = start.clone();
        let mut scratch = S::default();

        while !self.open.is_empty() {
            // Grab the one with the smallest total cost (the reference's
            // linear scan over the open set).
            let mut best_slot = 0;
            for slot in 1..self.open.len() {
                if cmp_f(
                    &self.nodes[self.open[slot]],
                    &self.nodes[self.open[best_slot]],
                ) {
                    best_slot = slot;
                }
            }
            let current = self.open.swap_remove(best_slot);

            // If it satisfies the goal requirements, we are done.
            if self.nodes[current].ext.is_goal(goal) {
                // The reference back-links pChild from the goal up to the
                // start and hands out first()/next(); the port walks the
                // parent chain and reverses it.
                let mut walk = current;
                self.path.clear();
                loop {
                    self.path.push(self.nodes[walk].ext.clone());
                    match self.nodes[walk].parent {
                        Some(parent) => walk = parent,
                        None => break,
                    }
                }
                self.path.reverse();
                return true;
            }

            // Now check all the adjacencies.
            for index in 0..self.nodes[current].ext.adjacency_count() {
                // "not every adjacency need be pursued"
                if !self.nodes[current].ext.get_adjacency(
                    index,
                    &mut scratch,
                    goal,
                    &mut self.search_data,
                ) {
                    continue;
                }

                // Don't return to the start state.
                if scratch.compare(&start_state) == 0 {
                    continue;
                }

                if check_max_dist && scratch.distance_to_goal(&start_state) > max_distance {
                    continue;
                }

                let g = self.nodes[current].g + scratch.distance_from_parent();
                let f = g + scratch.distance_to_goal(goal);

                let key = scratch.search_key();
                let adjacency = match self.close.get(&key) {
                    Some(&dup) => {
                        // Already visited: keep it when the old cost wins.
                        if self.nodes[dup].f <= f {
                            continue;
                        }
                        // Otherwise remove it and re-add it below with the
                        // smaller cost.
                        self.close.remove(&key);
                        if let Some(slot) = self.open.iter().position(|&s| s == dup) {
                            self.open.remove(slot);
                        }
                        dup
                    }
                    None => {
                        self.nodes.push(AStarNode::default());
                        self.nodes.len() - 1
                    }
                };

                // Sometimes they enter a loop; break it by refusing an
                // ancestor (the node stays unlinked, as in the reference).
                // With Euclidean h and positive steps a cycle back to an
                // ancestor never comes in under its recorded f, so the
                // reference's defensive guard stays unexercised here too.
                let mut walk = Some(current);
                let mut is_ancestor = false;
                while let Some(step) = walk {
                    if step == adjacency {
                        is_ancestor = true;
                        break;
                    }
                    walk = self.nodes[step].parent;
                }
                if is_ancestor {
                    continue;
                }

                self.nodes[adjacency] = AStarNode {
                    ext: scratch.clone(),
                    g,
                    f,
                    parent: Some(current),
                };

                // The set contains all states encountered; the open queue
                // all not yet fully expanded. Add to both.
                self.close.insert(key, adjacency);
                self.open.push(adjacency);
            }
        }

        false
    }

    /// `first()` (`astar.ipp:208-217`): the path's first state, or `None`
    /// when no search succeeded.
    pub fn first(&self) -> Option<&S> {
        self.path.first()
    }

    /// The whole path in start-to-goal order (`first()` + `next()` walks).
    pub fn path(&self) -> &[S] {
        &self.path
    }

    /// `searchData_`: the connections recorded while expanding.
    pub fn search_data(&self) -> &S::SearchData {
        &self.search_data
    }
}

#[cfg(test)]
impl<S: AStarState> AStar<S> {
    /// Plants a finished search result for the path classes' unit tests.
    pub fn set_path_for_test(&mut self, path: Vec<S>) {
        self.path = path;
    }

    /// The recorded connections, mutable, so a path unit test can plant
    /// the pairs `setConnection` would have written while expanding —
    /// the funnel's corridor shape then needs no live search.
    pub fn search_data_mut(&mut self) -> &mut S::SearchData {
        &mut self.search_data
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chunk_waypoint_set_data::{ChunkWaypointSetData, parse_world_navmesh};
    use crate::chunk_waypoint_state::ChunkWaypointState;
    use crate::navloc::NavLoc;
    use foundation::math::Vector3;
    use std::rc::Rc;

    // The fixture's three-waypoint set, straight off the truth log's
    // decode (SET0: square A, triangle C, triangle B).
    fn fixture_set() -> Rc<ChunkWaypointSetData> {
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
        put_i(&mut b, 3); // numWaypoints
        put_i(&mut b, 11); // numEdges
        // The waypoint header block lands first, then the edge stream
        // (the decode's two cursors).
        // WP0: square (0,0)-(2,2), heights 0..1.
        put_f(&mut b, 0.0);
        put_f(&mut b, 1.0);
        put_i(&mut b, 4);
        // WP1: triangle (2,2)-(4,0)-(2,0), heights 1..1.
        put_f(&mut b, 1.0);
        put_f(&mut b, 1.0);
        put_i(&mut b, 4);
        // WP2: triangle (4,2)-(4,0)-(2,2), heights 2..4.
        put_f(&mut b, 2.0);
        put_f(&mut b, 4.0);
        put_i(&mut b, 3);
        put_e(&mut b, 0.0, 2.0, 65535);
        put_e(&mut b, 2.0, 2.0, 65530);
        put_e(&mut b, 2.0, 0.0, 1);
        put_e(&mut b, 0.0, 0.0, 1);
        put_e(&mut b, 2.0, 2.0, 65533);
        put_e(&mut b, 2.0, 2.0, 0);
        put_e(&mut b, 4.0, 0.0, 2);
        put_e(&mut b, 2.0, 0.0, 2);
        put_e(&mut b, 4.0, 2.0, 65535);
        put_e(&mut b, 4.0, 0.0, 65528);
        put_e(&mut b, 2.0, 2.0, 1);

        let mut sets = parse_world_navmesh(&b).expect("decode");
        Rc::new(sets.remove(0))
    }

    fn state_on(
        set: &Rc<ChunkWaypointSetData>,
        waypoint: i32,
        x: f32,
        y: f32,
        z: f32,
    ) -> ChunkWaypointState {
        ChunkWaypointState::from_navloc(&NavLoc::from_parts(
            Rc::clone(set),
            waypoint,
            Vector3::new(x, y, z),
            0.0,
        ))
    }

    #[test]
    fn the_fixture_path_walks_waypoint_to_waypoint() {
        let set = fixture_set();
        let src = state_on(&set, 0, 0.5, 0.51, 0.5);
        let dst = state_on(&set, 1, 3.5, 0.51, 1.5);

        let mut astar = AStar::default();
        assert!(astar.search(&src, &dst, -1.0));

        let path = astar.path();
        assert_eq!(path.len(), 2, "start and goal states");
        assert_eq!(path[0].nav_loc().waypoint(), 0);
        assert_eq!(path[1].nav_loc().waypoint(), 1);
        // The crossing point from the truth log (PATH AC 2 1 0).
        let p = path[1].nav_loc().point();
        assert_eq!(p, Vector3::new(2.0, 1.0, 0.0));
        assert_eq!(astar.first().map(|s| s.nav_loc().waypoint()), Some(0));
    }

    #[test]
    fn the_two_hop_path_crosses_both_shared_edges() {
        let set = fixture_set();
        // BA: triangle B back to the square through C.
        let src = state_on(&set, 2, 3.5, 0.51, 3.0);
        let dst = state_on(&set, 0, 0.5, 0.51, 0.5);

        let mut astar = AStar::default();
        assert!(astar.search(&src, &dst, -1.0));

        let path = astar.path();
        assert_eq!(path.len(), 3);
        let waypoints: Vec<i32> = path.iter().map(|s| s.nav_loc().waypoint()).collect();
        assert_eq!(waypoints, [2, 1, 0]);
        // PATH BA steps 2 and 3.
        assert_eq!(path[1].nav_loc().point(), Vector3::new(2.0, 1.0, 2.0));
        assert_eq!(path[2].nav_loc().point(), Vector3::new(2.0, 1.0, 2.0));
    }

    #[test]
    fn an_unreachable_goal_answers_false_with_an_empty_path() {
        let set = fixture_set();
        // WP2 is only reachable through WP1's girth-filtered neighbours...
        // the square (10..12) far outside any adjacency: a waypoint -1
        // goal on the same set is reachable, so use a detached goal set.
        let other = {
            let mut b = Vec::new();
            let put_f = |b: &mut Vec<u8>, v: f32| b.extend_from_slice(&v.to_le_bytes());
            let put_i = |b: &mut Vec<u8>, v: i32| b.extend_from_slice(&v.to_le_bytes());
            put_i(&mut b, 0);
            put_f(&mut b, 0.6);
            put_i(&mut b, 1);
            put_i(&mut b, 4);
            put_f(&mut b, 0.0);
            put_f(&mut b, 1.0);
            put_i(&mut b, 4);
            // Single waypoint: header block then edge stream.
            for (x, y) in [(10.0f32, 10.0), (12.0, 10.0), (12.0, 12.0), (10.0, 12.0)] {
                put_f(&mut b, x);
                put_f(&mut b, y);
                put_i(&mut b, 65535);
            }
            let mut sets = parse_world_navmesh(&b).expect("decode");
            Rc::new(sets.remove(0))
        };
        let src = state_on(&set, 0, 0.5, 0.51, 0.5);
        let dst = state_on(&other, 0, 11.0, 0.51, 11.0);

        let mut astar = AStar::default();
        assert!(!astar.search(&src, &dst, -1.0));
        assert!(astar.path().is_empty());
    }

    #[test]
    fn the_start_state_is_never_returned_to() {
        let set = fixture_set();
        // The square's own adjacency loops through wp1 back to wp0; the
        // compare(start) == 0 guard refuses it and the search ends with the
        // start state alone in the open set.
        let src = state_on(&set, 0, 0.5, 0.51, 0.5);
        let dst = state_on(&set, 0, 1.5, 0.51, 1.5);

        let mut astar = AStar::default();
        assert!(astar.search(&src, &dst, -1.0));
        assert_eq!(astar.path().len(), 1);
        assert_eq!(astar.path()[0].nav_loc().waypoint(), 0);
    }

    #[test]
    fn the_distance_cap_refuses_far_adjacencies() {
        let set = fixture_set();
        // Every neighbour of wp0 sits well beyond 0.1 of the start.
        let src = state_on(&set, 0, 0.5, 0.51, 0.5);
        let dst = state_on(&set, 1, 3.5, 0.51, 1.5);

        let mut astar = AStar::default();
        assert!(!astar.search(&src, &dst, 0.1));
    }

    #[test]
    fn a_better_cost_replaces_a_visited_state() {
        let set = fixture_set();
        // BA has two routes into wp1 from wp2 (edges 2 and 3 both answer
        // wp1); the search keeps whichever expansion wins on f, and the
        // replay of the truth log pins the surviving connection point.
        let src = state_on(&set, 2, 3.5, 0.51, 3.0);
        let dst = state_on(&set, 0, 0.5, 0.51, 0.5);

        let mut astar = AStar::default();
        assert!(astar.search(&src, &dst, -1.0));
        assert_eq!(
            astar.path()[1].nav_loc().point(),
            Vector3::new(2.0, 1.0, 2.0)
        );
    }

    #[test]
    fn the_search_records_edge_connections() {
        let set = fixture_set();
        let src = state_on(&set, 0, 0.5, 0.51, 0.5);
        let dst = state_on(&set, 1, 3.5, 0.51, 1.5);

        let mut astar = AStar::default();
        assert!(astar.search(&src, &dst, -1.0));
        let (vp1, vp2) = astar.search_data().get_connection(&set, 0, &set, 1);
        // Both of wp0's edges to wp1 expand; the connection map is
        // last-writer-wins (setConnection runs inside getAdjacency, before
        // the close-set dup check), so the surviving pair is edge 3's.
        assert_eq!(vp1, 3, "edge 3 starts at vertex (0,0)");
        assert_eq!(vp2, 0, "and the next edge starts at vertex (0,2)");
    }

    #[test]
    fn the_f_tie_prefers_the_deeper_state() {
        // `cmp_f` (`astar.hpp:220-231`): f ties within the almostEqual
        // epsilon prefer the larger g, distinct f values compare plainly.
        let shallow = AStarNode::<ChunkWaypointState> {
            f: 1.0,
            g: 0.5,
            ..AStarNode::default()
        };
        let deep = AStarNode::<ChunkWaypointState> {
            f: 1.0 + 1e-6,
            g: 2.0,
            ..AStarNode::default()
        };
        assert!(cmp_f(&deep, &shallow), "the tied pair takes the larger g");
        assert!(!cmp_f(&shallow, &deep));

        let far = AStarNode::<ChunkWaypointState> {
            f: 5.0,
            ..AStarNode::default()
        };
        assert!(cmp_f(&deep, &far), "the smaller f wins outright");
        assert!(!cmp_f(&far, &deep));
    }

    // A three-strip map: wp0 the pentagon square, wp1 and wp2 the upper
    // and lower east triangles. Expanding wp0 pushes wp2 first (edge 1)
    // and wp1 second (edge 2) with the smaller f, so the min-f scan must
    // pass over slot 0.
    fn three_strip_set() -> Rc<ChunkWaypointSetData> {
        let mut b = Vec::new();
        let put_f = |b: &mut Vec<u8>, v: f32| b.extend_from_slice(&v.to_le_bytes());
        let put_i = |b: &mut Vec<u8>, v: i32| b.extend_from_slice(&v.to_le_bytes());
        let put_e = |b: &mut Vec<u8>, x: f32, y: f32, n: i32| {
            put_f(b, x);
            put_f(b, y);
            put_i(b, n);
        };

        put_i(&mut b, 0);
        put_f(&mut b, 0.6);
        put_i(&mut b, 3);
        put_i(&mut b, 11);
        // wp0: pentagon (0,2)-(2,2)-(2,1)-(2,0)-(0,0), heights 0..1.
        put_f(&mut b, 0.0);
        put_f(&mut b, 1.0);
        put_i(&mut b, 5);
        // wp1: triangle (2,1)-(4,1)-(2,0), heights 1..1.
        put_f(&mut b, 1.0);
        put_f(&mut b, 1.0);
        put_i(&mut b, 3);
        // wp2: triangle (2,2)-(4,2)-(2,1), heights 1..1.
        put_f(&mut b, 1.0);
        put_f(&mut b, 1.0);
        put_i(&mut b, 3);
        for (x, y, n) in [
            (0.0f32, 2.0, 65535),
            (2.0, 2.0, 2),
            (2.0, 1.0, 1),
            (2.0, 0.0, 65535),
            (0.0, 0.0, 65535),
            (2.0, 1.0, 65535),
            (4.0, 1.0, 65535),
            (2.0, 0.0, 0),
            (2.0, 2.0, 65535),
            (4.0, 2.0, 65535),
            (2.0, 1.0, 0),
        ] {
            put_e(&mut b, x, y, n);
        }

        let mut sets = parse_world_navmesh(&b).expect("decode");
        Rc::new(sets.remove(0))
    }

    #[test]
    fn the_min_f_scan_passes_over_the_insertion_order() {
        let set = three_strip_set();
        // wp2 enters the open set first but wp1's f comes out smaller, so
        // wp1 pops first and burns its only useful adjacency (back to the
        // start) before wp2 answers the goal.
        let src = state_on(&set, 0, 1.0, 0.51, 1.0);
        let dst = state_on(&set, 2, 3.5, 0.51, 1.5);

        let mut astar = AStar::default();
        assert!(astar.search(&src, &dst, -1.0));
        let waypoints: Vec<i32> = astar
            .path()
            .iter()
            .map(|s| s.nav_loc().waypoint())
            .collect();
        assert_eq!(waypoints, [0, 2], "wp1 expanded first, then wp2");
    }

    // An L-shaped map: wp0's edges 2 and 3 both border wp1, and the line
    // to the goal crosses only edge 3's span — the second adjacency lands
    // strictly cheaper and must replace the visited state.
    fn l_shape_set() -> Rc<ChunkWaypointSetData> {
        let mut b = Vec::new();
        let put_f = |b: &mut Vec<u8>, v: f32| b.extend_from_slice(&v.to_le_bytes());
        let put_i = |b: &mut Vec<u8>, v: i32| b.extend_from_slice(&v.to_le_bytes());
        let put_e = |b: &mut Vec<u8>, x: f32, y: f32, n: i32| {
            put_f(b, x);
            put_f(b, y);
            put_i(b, n);
        };

        put_i(&mut b, 0);
        put_f(&mut b, 0.6);
        put_i(&mut b, 2);
        put_i(&mut b, 8);
        // wp0: the same pentagon, both east edges bordering wp1.
        put_f(&mut b, 0.0);
        put_f(&mut b, 1.0);
        put_i(&mut b, 5);
        // wp1: triangle (2,1)-(2,0)-(0,0), heights 1..1.
        put_f(&mut b, 1.0);
        put_f(&mut b, 1.0);
        put_i(&mut b, 3);
        for (x, y, n) in [
            (0.0f32, 2.0, 65535),
            (2.0, 2.0, 65535),
            (2.0, 1.0, 1),
            (2.0, 0.0, 1),
            (0.0, 0.0, 65535),
            (2.0, 1.0, 0),
            (2.0, 0.0, 0),
            (0.0, 0.0, 65535),
        ] {
            put_e(&mut b, x, y, n);
        }

        let mut sets = parse_world_navmesh(&b).expect("decode");
        Rc::new(sets.remove(0))
    }

    #[test]
    fn the_cheaper_late_adjacency_replaces_the_open_state() {
        let set = l_shape_set();
        // Edge 2's endpoint answer (2,1) enters wp1 first; edge 3's
        // crossing at (1,0) is strictly closer to the goal, so the visited
        // wp1 must come off the open set and re-enter with the new point.
        let src = state_on(&set, 0, 1.0, 0.51, 1.0);
        let dst = state_on(&set, 1, 1.0, 1.0, 0.2);

        let mut astar = AStar::default();
        assert!(astar.search(&src, &dst, -1.0));
        assert_eq!(astar.path().len(), 2);
        assert_eq!(
            astar.path()[1].nav_loc().point(),
            Vector3::new(1.0, 1.0, 0.0),
            "the surviving state carries the cheaper crossing"
        );
    }
}
