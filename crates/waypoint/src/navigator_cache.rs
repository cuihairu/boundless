//! `lib/waypoint/navigator_cache.{hpp,cpp}` — the two cached search
//! results a Navigator keeps: the in-set waypoint path and the waypoint
//! set path. `findWayPath`/`findWaySetPath` are the cache-hit tests
//! (`matches`); the saves run after a fresh search.
//!
//! Registered boundary (smoothing face deferred, see
//! `chunk_waypoint_state_path`): the reference's `saveWayPath` also calls
//! `smoothPath` between `init` and `removeDuplicates`; in this lane's
//! truth world every way path is 2–3 states and `smoothPath` early-returns
//! untouched, so the saved paths are identical without it.

use super::astar::AStar;
use super::chunk_waypoint_set_state::ChunkWPSetState;
use super::chunk_waypoint_set_state_path::ChunkWaypointSetStatePath;
use super::chunk_waypoint_state::ChunkWaypointState;
use super::chunk_waypoint_state_path::ChunkWaypointStatePath;
use super::search_path_base::SearchPathBase;

/// `NavigatorCache` (`navigator_cache.hpp:19-94`).
#[derive(Debug, Default)]
pub struct NavigatorCache {
    /// `wayPath_`.
    way_path: ChunkWaypointStatePath,
    /// `waySetPath_`.
    way_set_path: ChunkWaypointSetStatePath,
}

impl NavigatorCache {
    /// `saveWayPath` (`navigator_cache.cpp:31-41`): initialise from the
    /// unspoilt search, then dedupe. The `smoothPath` call sits deferred
    /// between the two (see the module header) — its `src`/`dst` feeding
    /// is why the reference signature carries them.
    pub fn save_way_path(
        &mut self,
        astar: &AStar<ChunkWaypointState>,
        _src: &ChunkWaypointState,
        _dst: &ChunkWaypointState,
    ) {
        self.way_path.init(astar);
        // wayPath_.smoothPath( astar, src, dst ) — deferred.
        self.way_path.remove_duplicates();
        debug_assert!(self.way_path.is_valid());
    }

    /// `wayPath()`.
    pub fn way_path(&self) -> &ChunkWaypointStatePath {
        &self.way_path
    }

    /// `findWayPath`: the cache-hit test, advancing over the path when
    /// the source is the next state.
    pub fn find_way_path(&mut self, src: &ChunkWaypointState, dst: &ChunkWaypointState) -> bool {
        self.way_path.matches(src, dst)
    }

    /// `pNextWaypointState()`.
    pub fn p_next_waypoint_state(&self) -> Option<&ChunkWaypointState> {
        self.way_path.p_next()
    }

    /// `clearWayPath()`.
    pub fn clear_way_path(&mut self) {
        self.way_path.clear();
    }

    /// `moveStateAlongPath()`.
    pub fn move_state_along_path(&mut self, state: &ChunkWaypointState) {
        self.way_path.move_state_along_path(state);
    }

    /// `saveWaySetPath` (`navigator_cache.cpp:69-75`).
    pub fn save_way_set_path(&mut self, astar: &AStar<ChunkWPSetState>) {
        self.way_set_path.init(astar);
        debug_assert!(self.way_set_path.is_valid());
    }

    /// `findWaySetPath`.
    pub fn find_way_set_path(&mut self, src: &ChunkWPSetState, dst: &ChunkWPSetState) -> bool {
        self.way_set_path.matches(src, dst)
    }

    /// `pNextWaypointSetState()`.
    pub fn p_next_waypoint_set_state(&self) -> Option<&ChunkWPSetState> {
        self.way_set_path.p_next()
    }

    /// `clearWaySetPath()` — necessary because the set answer depends on
    /// `blockNonPermissive`.
    pub fn clear_way_set_path(&mut self) {
        self.way_set_path.clear();
    }

    /// `getWaySetPathSize()`.
    pub fn get_way_set_path_size(&self) -> usize {
        self.way_set_path.size()
    }

    /// `waySetPathPassedShellBoundary()`.
    pub fn way_set_path_passed_shell_boundary(&self) -> bool {
        self.way_set_path.passed_shell_boundary()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chunk_waypoint_set_data::parse_world_navmesh;
    use crate::navloc::NavLoc;
    use crate::search_path_base::SearchPathBase;
    use foundation::math::Vector3;
    use std::rc::Rc;

    fn one_set() -> Rc<crate::chunk_waypoint_set_data::ChunkWaypointSetData> {
        let mut b = Vec::new();
        let put_f = |b: &mut Vec<u8>, v: f32| b.extend_from_slice(&v.to_le_bytes());
        let put_i = |b: &mut Vec<u8>, v: i32| b.extend_from_slice(&v.to_le_bytes());
        put_i(&mut b, 0);
        put_f(&mut b, 0.6);
        put_i(&mut b, 1);
        put_i(&mut b, 4);
        put_f(&mut b, 0.2);
        put_f(&mut b, 0.6);
        put_i(&mut b, 4);
        for (x, y) in [(0.0f32, 0.0), (0.0, 1.0), (1.0, 1.0), (1.0, 0.0)] {
            put_f(&mut b, x);
            put_f(&mut b, y);
            put_i(&mut b, 65535);
        }
        Rc::new(parse_world_navmesh(&b).expect("decode").remove(0))
    }

    fn wp_state(
        set: &Rc<crate::chunk_waypoint_set_data::ChunkWaypointSetData>,
        point: Vector3,
    ) -> ChunkWaypointState {
        ChunkWaypointState::from_navloc(&NavLoc::from_parts(Rc::clone(set), 0, point))
    }

    /// A three-state walk with no duplicate points, no colinear middle
    /// and three distinct sets, so `removeDuplicates` leaves it whole and
    /// no two states are waypoint-equivalent.
    fn walk(
        set_a: &Rc<crate::chunk_waypoint_set_data::ChunkWaypointSetData>,
        set_b: &Rc<crate::chunk_waypoint_set_data::ChunkWaypointSetData>,
        set_c: &Rc<crate::chunk_waypoint_set_data::ChunkWaypointSetData>,
    ) -> Vec<ChunkWaypointState> {
        vec![
            wp_state(set_a, Vector3::new(0.1, 0.4, 0.5)),
            wp_state(set_b, Vector3::new(0.3, 0.4, 0.8)),
            wp_state(set_c, Vector3::new(0.7, 0.4, 0.5)),
        ]
    }

    #[test]
    fn the_cache_saves_finds_and_advances_the_way_path() {
        let set_a = one_set();
        let set_b = one_set();
        let set_c = one_set();
        let states = walk(&set_a, &set_b, &set_c);
        let (a, b, c) = (&states[0], &states[1], &states[2]);

        let mut cache = NavigatorCache::default();
        let mut astar = AStar::default();
        astar.set_path_for_test(states.clone());
        cache.save_way_path(&astar, a, c);

        // The next state is the middle state; a source there is a hit.
        assert!(Rc::ptr_eq(
            cache
                .p_next_waypoint_state()
                .unwrap()
                .nav_loc()
                .p_set()
                .unwrap(),
            &set_b
        ));
        assert!(cache.find_way_path(b, c));
        // The hit popped the current state off.
        assert!(cache.way_path().is_current_state(b));
        // A fresh save resets; the destination state as source is then
        // only a hit through the full path (no advance).
        let mut astar = AStar::default();
        astar.set_path_for_test(states.clone());
        cache.save_way_path(&astar, a, c);
        assert!(cache.find_way_path(c, c), "isPartOfPath arm");
        assert!(cache.way_path().is_current_state(a), "no advance");

        // Another fresh save; a source on the current state is then a hit
        // without advancing (the isCurrentState arm).
        let mut astar = AStar::default();
        astar.set_path_for_test(states.clone());
        cache.save_way_path(&astar, a, c);
        assert!(cache.find_way_path(a, c));
        assert!(cache.way_path().is_current_state(a), "no advance");

        // The cache-level move walks the way path's cursor.
        cache.move_state_along_path(b);
        assert!(cache.way_path().is_part_of_path(b));
        assert!(!cache.way_path().is_part_of_path(a), "behind the cursor");

        // A destination change refuses.
        let foreign = wp_state(&one_set(), Vector3::new(0.5, 0.4, 0.5));
        assert!(!cache.find_way_path(a, &foreign));

        cache.clear_way_path();
        assert!(!cache.way_path().is_valid());
        assert!(cache.p_next_waypoint_state().is_none());
    }

    #[test]
    fn the_set_path_cache_reports_size_and_shell() {
        let mut cache = NavigatorCache::default();
        assert_eq!(cache.get_way_set_path_size(), 0);
        assert!(!cache.way_set_path_passed_shell_boundary());

        let mut astar = AStar::default();
        // The reference debug-asserts the saved set path is valid, so the
        // walk carries at least a current and a destination state.
        astar.set_path_for_test(vec![
            ChunkWPSetState::from_navloc(&NavLoc::from_parts(
                one_set(),
                0,
                Vector3::new(0.0, 0.4, 0.5),
            )),
            ChunkWPSetState::from_navloc(&NavLoc::from_parts(
                one_set(),
                0,
                Vector3::new(1.0, 0.4, 0.5),
            )),
        ]);
        cache.save_way_set_path(&astar);
        assert_eq!(cache.get_way_set_path_size(), 2);
        cache.clear_way_set_path();
        assert_eq!(cache.get_way_set_path_size(), 0);
    }
}
