//! `lib/waypoint/chunk_waypoint_state_path.{hpp,cpp}` — the per-set
//! waypoint path: the chunk-level A* result reversed into `pop` order,
//! with the full-path cursor the cache nudges as its owner moves.
//!
//! Registered boundary (smoothing face deferred): the reference's
//! `saveWayPath` also runs `smoothPath` (string-pulling/funnel) before
//! `removeDuplicates`. The funnel is a found-path point-shaping face with
//! no effect on reachable/unreachable, and in this lane's truth world the
//! way paths are 2–3 states, which `smoothPath` early-returns from
//! untouched (`reversePath_.size() == 2`) — so the port's cached paths are
//! byte-identical to the reference's on every pinned probe. The funnel
//! itself lands with the path-quality face that first needs it.

use super::astar::AStar;
use super::chunk_waypoint_state::ChunkWaypointState;
use super::girth::almost_equal;
use super::navloc::NavLoc;
use super::search_path_base::SearchPathBase;
use foundation::math::Vector3;
use std::rc::Rc;

/// `almostEqual( const Vector3 & )` (`cstdmf/stdmf.hpp:616` family, the
/// 0.0004f default epsilon, per component).
fn almost_equal_vec3(a: Vector3, b: Vector3) -> bool {
    almost_equal(a.x, b.x) && almost_equal(a.y, b.y) && almost_equal(a.z, b.z)
}

/// `ChunkWaypointStatePath`
/// (`chunk_waypoint_state_path.hpp:15-99`).
#[derive(Debug, Default)]
pub struct ChunkWaypointStatePath {
    /// `reversePath_` (the base's storage, flattened in).
    reverse_path: Vec<ChunkWaypointState>,
    /// `canBeSmoothed_`.
    can_be_smoothed: bool,
    /// `fullPath_`: the path as initialised, before any pop.
    full_path: Vec<ChunkWaypointState>,
    /// `stateIterOnFullPath_`: a reverse iterator into `full_path_` —
    /// index 0 is the newest (back) state, `full_path.len()` is rend.
    state_iter_on_full_path: usize,
}

impl SearchPathBase for ChunkWaypointStatePath {
    type State = ChunkWaypointState;

    fn reverse_path(&self) -> &[ChunkWaypointState] {
        &self.reverse_path
    }

    fn clear_reverse_path(&mut self) {
        self.reverse_path.clear();
    }

    fn pop_back(&mut self) {
        self.reverse_path.pop();
    }

    /// `statesAreEquivalent`: a portal state (waypoint -1) matches by set
    /// alone; everything else by `waypointsEqual`.
    fn states_are_equivalent(&self, s1: &ChunkWaypointState, s2: &ChunkWaypointState) -> bool {
        // If either is a portal, then accept it being equal if same set
        if s1.nav_loc().waypoint() == -1 || s2.nav_loc().waypoint() == -1 {
            return match (s1.nav_loc().p_set(), s2.nav_loc().p_set()) {
                (Some(a), Some(b)) => Rc::ptr_eq(a, b),
                (None, None) => true,
                _ => false,
            };
        }
        s1.nav_loc().waypoints_equal(s2.nav_loc())
    }

    /// `isPartOfPath`: any state on the full path from the cursor on.
    fn is_part_of_path(&self, current: &ChunkWaypointState) -> bool {
        let len = self.full_path.len();
        for offset in self.state_iter_on_full_path..len {
            let iter = &self.full_path[len - 1 - offset];
            if self.states_are_equivalent(current, iter) {
                return true;
            }
        }
        false
    }
}

impl ChunkWaypointStatePath {
    /// `init` (`chunk_waypoint_state_path.cpp:11-54`): reverse the A*
    /// walk, duplicating the last state when the walk is a single node
    /// (so `isValid` holds), and seed the full-path cursor.
    pub fn init(&mut self, astar: &AStar<ChunkWaypointState>) {
        self.reverse_path.clear();

        let mut forward_path: Vec<ChunkWaypointState> = astar.path().to_vec();

        // make sure that forwardPath has at least 2 nodes
        if forward_path.len() < 2
            && let Some(last) = forward_path.last()
        {
            let last = last.clone();
            forward_path.push(last);
        }

        // and store the path in reverse order (for pop_back)
        self.reverse_path = forward_path.iter().rev().cloned().collect();

        self.full_path = self.reverse_path.clone();
        self.state_iter_on_full_path = 0;

        self.can_be_smoothed = true;
    }

    /// `canBeSmoothed_` (the smoothing face is deferred; see the module
    /// header — the flag still tracks the reference's lifecycle).
    pub fn can_be_smoothed(&self) -> bool {
        self.can_be_smoothed
    }

    /// `fullPath()`.
    pub fn full_path(&self) -> &[ChunkWaypointState] {
        &self.full_path
    }

    /// `removeDuplicates` (`chunk_waypoint_state_path.cpp:62-108`): drop
    /// repeated points and the middle of three colinear points (the
    /// two-endpoint guards keep a 2-state path intact).
    pub fn remove_duplicates(&mut self) {
        debug_assert!(self.reverse_path.len() >= 2);
        let mut path_index = 1;

        while path_index != self.reverse_path.len() {
            let current = self.reverse_path[path_index].nav_loc().point();
            let prev = self.reverse_path[path_index - 1].nav_loc().point();

            let mut is_point_unnecessary = false;

            // Duplicate point
            if almost_equal_vec3(current, prev) {
                is_point_unnecessary = true;
            }

            // The centre of three colinear points
            if path_index != self.reverse_path.len() - 1 {
                let next = self.reverse_path[path_index + 1].nav_loc().point();

                let mut from_prev = current - prev;
                let mut to_next = next - current;

                from_prev.normalise();
                to_next.normalise();

                if almost_equal_vec3(from_prev, to_next) {
                    is_point_unnecessary = true;
                }
            }

            if is_point_unnecessary && self.reverse_path.len() > 2 {
                self.reverse_path.remove(path_index);
                self.can_be_smoothed = false;
            } else {
                path_index += 1;
            }
        }
    }

    /// `validateNavLoc` (`chunk_waypoint_state_path.cpp:593-627`): a
    /// located NavLoc on the cached path for `position`, the next state
    /// first, then the full path from the cursor. The reference's
    /// `NavLoc(guess, point)` fallback arms (waypoint -1 guesses through
    /// `pSpace->collide`, and the relocate-through-space on a find miss)
    /// stay registered-deferred; the port's `from_guess` covers the
    /// contains-projection and set-find arms.
    pub fn validate_navloc(&self, position: Vector3) -> Option<NavLoc> {
        if !self.is_valid() {
            return None;
        }

        if let Some(next_state) = self.p_next()
            && next_state.nav_loc().contains_projection(position)
        {
            return Some(self.locate_on(next_state, position));
        }

        let len = self.full_path.len();
        for offset in self.state_iter_on_full_path..len {
            let iter = &self.full_path[len - 1 - offset];
            if iter.nav_loc().contains_projection(position) {
                return Some(self.locate_on(iter, position));
            }
        }
        None
    }

    /// `NavLoc( const NavLoc & guess, const Vector3 & point )` against a
    /// path state's set and grid size.
    fn locate_on(&self, state: &ChunkWaypointState, position: Vector3) -> NavLoc {
        let guess = state.nav_loc();
        let set = Rc::clone(guess.p_set().expect("path states are located"));
        NavLoc::from_guess(guess, set, guess.grid_size(), position)
    }

    /// `moveStateAlongPath` (`chunk_waypoint_state_path.cpp:633-671`):
    /// skip the cursor forward to `state`, stopping at the next state.
    /// The reference dereferences a past-`rend` cursor when nothing
    /// matches (unreachable through the navigator's live paths); the port
    /// clamps those reads to early returns instead.
    pub fn move_state_along_path(&mut self, state: &ChunkWaypointState) {
        let Some(next_state) = self.p_next().cloned() else {
            // No state, path is probably empty
            return;
        };

        let len = self.full_path.len();
        let full = |offset: usize| &self.full_path[len - 1 - offset];

        // Skip over states till one matches the current state, or we
        // reach the next state.
        while self.state_iter_on_full_path < len
            && !self.states_are_equivalent(state, full(self.state_iter_on_full_path))
            && !self.states_are_equivalent(full(self.state_iter_on_full_path), &next_state)
        {
            self.state_iter_on_full_path += 1;
        }

        let next_on_full_path = self.state_iter_on_full_path + 1;

        // We have reached the next state, this is the state we are meant
        // to reach.
        if (self.state_iter_on_full_path < len
            && self.states_are_equivalent(full(self.state_iter_on_full_path), &next_state))
            || next_on_full_path >= len
        {
            return;
        }

        // To prevent being stuck on a state which overlaps an existing
        // state we check if the next state could be matched by waypoint
        // (statesAreEquivalent) or point.
        if self.states_are_equivalent(state, full(next_on_full_path))
            || almost_equal_vec3(
                full(next_on_full_path).nav_loc().point(),
                state.nav_loc().point(),
            )
        {
            self.state_iter_on_full_path = next_on_full_path;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chunk_waypoint_set_data::parse_world_navmesh;
    use crate::navloc::NavLoc;

    /// One single-waypoint square set per call.
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

    fn state(
        set: &Rc<crate::chunk_waypoint_set_data::ChunkWaypointSetData>,
        x: f32,
    ) -> ChunkWaypointState {
        ChunkWaypointState::from_navloc(&NavLoc::from_parts(
            Rc::clone(set),
            0,
            Vector3::new(x, 0.4, 0.5),
            0.0,
        ))
    }

    #[test]
    fn a_single_node_walk_is_duplicated_to_stay_valid() {
        let set = one_set();
        let lone = state(&set, 0.5);
        let mut astar = AStar::default();
        astar.set_path_for_test(vec![lone.clone()]);

        let mut path = ChunkWaypointStatePath::default();
        path.init(&astar);

        assert_eq!(path.reverse_path().len(), 2, "the pLast duplicate");
        assert!(path.is_valid());
        assert!(path.is_current_state(&lone));
        assert!(path.is_destination_state(&lone));
        assert!(path.can_be_smoothed());
    }

    #[test]
    fn the_portal_state_matches_by_set_alone() {
        let set = one_set();
        // A waypoint -1 state is the reference's portal arm: it matches a
        // located state on the same set (and only that).
        let portal = ChunkWaypointState::from_navloc(&NavLoc::from_parts(
            Rc::clone(&set),
            -1,
            Vector3::new(0.3, 0.4, 0.5),
            0.0,
        ));
        let located = state(&set, 0.7);
        let other = state(&one_set(), 0.7);

        let mut path = ChunkWaypointStatePath::default();
        path.init(&{
            let mut astar = AStar::default();
            astar.set_path_for_test(vec![located.clone(), portal.clone()]);
            astar
        });
        assert!(path.states_are_equivalent(&portal, &located));
        assert!(!path.states_are_equivalent(&portal, &other));
        assert!(path.is_destination_state(&located));

        // A setless location never matches a located one (the match arms'
        // mixed-set case).
        let setless = ChunkWaypointState::from_navloc(&NavLoc::new());
        assert!(!path.states_are_equivalent(&portal, &setless));
    }

    #[test]
    fn the_accessors_and_guards_answer_without_a_live_walk() {
        let set = one_set();
        let a = state(&set, 0.1);
        let b = state(&set, 0.7);
        let mut path = ChunkWaypointStatePath::default();
        path.init(&{
            let mut astar = AStar::default();
            astar.set_path_for_test(vec![a.clone(), b.clone()]);
            astar
        });

        // fullPath() is the pre-pop snapshot; nothing popped here.
        assert_eq!(path.full_path().len(), 2);

        // An invalid path answers nothing, and moving along it is a no-op.
        path.clear();
        assert!(path.validate_navloc(Vector3::new(0.5, 0.4, 0.5)).is_none());
        path.move_state_along_path(&a);
    }

    #[test]
    fn an_overlapping_state_nudges_the_cursor_forward() {
        let set_a = one_set();
        let set_b = one_set();
        let a = state(&set_a, 0.1);
        let b = state(&set_b, 0.3);
        let c = state(&one_set(), 0.7);
        let mut path = ChunkWaypointStatePath::default();
        path.init(&{
            let mut astar = AStar::default();
            astar.set_path_for_test(vec![a.clone(), b.clone(), c.clone()]);
            astar
        });

        // A portal state parked on the source state's set and sitting on
        // the next state's exact point: the loop stops on the source (set
        // equality), and the point check advances the cursor over the
        // overlapping state.
        let parked = ChunkWaypointState::from_navloc(&NavLoc::from_parts(
            Rc::clone(&set_a),
            -1,
            Vector3::new(0.3, 0.4, 0.5),
            0.0,
        ));
        path.move_state_along_path(&parked);
        assert!(!path.is_part_of_path(&a), "the source fell behind");
        assert!(path.is_part_of_path(&b));
    }

    #[test]
    fn remove_duplicates_drops_repeats_and_colinear_middles() {
        let set = one_set();
        // Three states: (0.1, 0.4, 0.5), a duplicate of it, then (0.5,
        // 0.4, 0.5) — the middle drops as a duplicate.
        let a = state(&set, 0.1);
        let dup = state(&set, 0.1);
        let b = state(&set, 0.5);
        let mut path = ChunkWaypointStatePath::default();
        path.init(&{
            let mut astar = AStar::default();
            astar.set_path_for_test(vec![a.clone(), dup.clone(), b.clone()]);
            astar
        });
        path.remove_duplicates();
        assert_eq!(path.reverse_path().len(), 2);
        assert!(!path.can_be_smoothed(), "the erasure clears the flag");

        // Colinear: (0.1,·), (0.3,·), (0.5,·) along one line — the
        // middle drops.
        let a = state(&set, 0.1);
        let mid = state(&set, 0.3);
        let b = state(&set, 0.5);
        let mut path = ChunkWaypointStatePath::default();
        path.init(&{
            let mut astar = AStar::default();
            astar.set_path_for_test(vec![a.clone(), mid, b.clone()]);
            astar
        });
        path.remove_duplicates();
        assert_eq!(path.reverse_path().len(), 2);
        assert!(path.is_destination_state(&b));

        // The two-endpoint guards keep a two-state path intact.
        let mut path = ChunkWaypointStatePath::default();
        path.init(&{
            let mut astar = AStar::default();
            astar.set_path_for_test(vec![a.clone(), a.clone()]);
            astar
        });
        path.remove_duplicates();
        assert_eq!(path.reverse_path().len(), 2, "size > 2 guard");
    }

    #[test]
    fn validate_navloc_walks_the_next_state_then_the_full_path() {
        let set = one_set();
        let a = state(&set, 0.1);
        let b = state(&set, 0.7);
        let mut path = ChunkWaypointStatePath::default();
        path.init(&{
            let mut astar = AStar::default();
            astar.set_path_for_test(vec![a.clone(), b.clone()]);
            astar
        });

        // Inside the next (destination) state's waypoint: the first arm.
        let located = path
            .validate_navloc(Vector3::new(0.65, 0.4, 0.5))
            .expect("hit");
        assert_eq!(located.waypoint(), 0);
        assert!((located.point().y - 0.41).abs() <= 1e-5, "the +0.01 lift");

        // Outside it: the full-path walk answers the source state.
        let located = path
            .validate_navloc(Vector3::new(0.12, 0.4, 0.5))
            .expect("hit");
        assert_eq!(located.waypoint(), 0);
        assert!((located.point().x - 0.12).abs() <= 1e-5);

        // Nowhere: none.
        assert!(
            path.validate_navloc(Vector3::new(50.0, 0.4, 50.0))
                .is_none()
        );
    }

    #[test]
    fn move_state_along_path_advances_the_cursor() {
        let set_a = one_set();
        let set_b = one_set();
        // Distinct points keep the reference's "overlapping state" nudge
        // arm out of the way; distinct sets keep the three states
        // pairwise inequivalent (waypointsEqual is per waypoint index).
        let a = state(&set_a, 0.1);
        let b = state(&set_b, 0.3);
        let c = state(&one_set(), 0.7);
        let mut path = ChunkWaypointStatePath::default();
        path.init(&{
            let mut astar = AStar::default();
            astar.set_path_for_test(vec![a.clone(), b.clone(), c.clone()]);
            astar
        });

        // The current state leaves the cursor where it is.
        path.move_state_along_path(&a);
        assert!(path.is_part_of_path(&a));
        assert!(path.is_current_state(&a));

        // A state ahead moves the cursor onto it: the states behind it
        // leave the path walk, while the current state does not move
        // (pCurrent only advances by pop).
        path.move_state_along_path(&b);
        assert!(path.is_part_of_path(&b));
        assert!(!path.is_part_of_path(&a), "behind the cursor");
        assert!(path.is_current_state(&a));

        // A state off the path leaves the cursor alone.
        let unknown = state(&one_set(), 0.9);
        path.move_state_along_path(&unknown);
        assert!(path.is_part_of_path(&b));
        assert!(!path.is_part_of_path(&a), "behind the cursor");
    }
}
