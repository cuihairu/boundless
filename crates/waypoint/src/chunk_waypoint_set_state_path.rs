//! `lib/waypoint/chunk_waypoint_set_state_path.{hpp,cpp}` — the waypoint
//! set path: the set-level A* result reversed into `pop` order, plus the
//! shell-boundary flag collected along the way. This is what
//! `Navigator::searchNextWaypointSet` stores per search.

use super::astar::AStar;
use super::chunk_waypoint_set_state::ChunkWPSetState;
use super::search_path_base::SearchPathBase;

/// `ChunkWaypointSetStatePath`
/// (`chunk_waypoint_set_state_path.hpp:13-52`).
#[derive(Debug, Default)]
pub struct ChunkWaypointSetStatePath {
    /// `reversePath_` (the base's storage, flattened in).
    reverse_path: Vec<ChunkWPSetState>,
    /// `passedShellBoundary_`.
    passed_shell_boundary: bool,
}

impl SearchPathBase for ChunkWaypointSetStatePath {
    type State = ChunkWPSetState;

    fn reverse_path(&self) -> &[ChunkWPSetState] {
        &self.reverse_path
    }

    fn clear_reverse_path(&mut self) {
        self.reverse_path.clear();
    }

    fn pop_back(&mut self) {
        self.reverse_path.pop();
    }

    /// `statesAreEquivalent`: the same waypoint set.
    fn states_are_equivalent(&self, s1: &ChunkWPSetState, s2: &ChunkWPSetState) -> bool {
        match (s1.p_set(), s2.p_set()) {
            (Some(a), Some(b)) => std::rc::Rc::ptr_eq(a, b),
            (None, None) => true,
            _ => false,
        }
    }
}

impl ChunkWaypointSetStatePath {
    /// `init` (`chunk_waypoint_set_state_path.cpp:13-43`): reverse the
    /// A* walk, OR-ing the shell-boundary flag over every state. Unlike
    /// the waypoint state path there is no under-two-nodes special case.
    pub fn init(&mut self, astar: &AStar<ChunkWPSetState>) {
        self.clear();
        self.passed_shell_boundary = false;

        self.reverse_path = astar
            .path()
            .iter()
            .rev()
            .inspect(|state| {
                // See if we crossed between an inside chunk and an
                // outside chunk anywhere along this path.
                self.passed_shell_boundary |= state.passed_shell_boundary();
            })
            .cloned()
            .collect();
    }

    /// `size()`.
    pub fn size(&self) -> usize {
        self.reverse_path.len()
    }

    /// `passedShellBoundary()`.
    pub fn passed_shell_boundary(&self) -> bool {
        self.passed_shell_boundary
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::navloc::NavLoc;
    use crate::search_path_base::SearchPathBase;
    use foundation::math::Vector3;
    use std::rc::Rc;

    /// One single-waypoint square set per call, so the three chain states
    /// sit on three distinct sets.
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
        Rc::new(
            crate::chunk_waypoint_set_data::parse_world_navmesh(&b)
                .expect("decode")
                .remove(0),
        )
    }

    /// A source-middle-destination chain; the middle state crossed a
    /// shell boundary.
    fn chain() -> Vec<ChunkWPSetState> {
        (0..3)
            .map(|index| {
                let state = ChunkWPSetState::from_navloc(&NavLoc::from_parts(
                    one_set(),
                    0,
                    Vector3::new(index as f32, 0.0, 0.0),
                ));
                let mut state = state;
                if index == 1 {
                    state.set_passed_shell_boundary_for_test(true);
                }
                state
            })
            .collect()
    }

    #[test]
    fn the_set_path_reverses_the_walk_and_collects_the_shell_flag() {
        let states = chain();
        let mut astar = AStar::default();
        astar.set_path_for_test(states.clone());

        let mut path = ChunkWaypointSetStatePath::default();
        path.init(&astar);

        // Reversed: destination first, source last.
        assert_eq!(path.size(), 3);
        assert!(Rc::ptr_eq(
            path.p_dest().unwrap().p_set().unwrap(),
            states[2].p_set().unwrap()
        ));
        assert!(Rc::ptr_eq(
            path.p_current().unwrap().p_set().unwrap(),
            states[0].p_set().unwrap()
        ));
        // The middle state crossed a shell boundary.
        assert!(path.passed_shell_boundary());

        // pNext/pCurrent/pDest and pop: the source state pops off.
        assert!(path.is_current_state(&states[0]));
        assert!(path.is_next_state(&states[1]));
        assert!(path.is_destination_state(&states[2]));
        path.pop();
        assert!(!path.is_current_state(&states[0]));
        assert!(path.is_current_state(&states[1]));

        // matches: the next state advances; a destination change refuses.
        // (The same states — a fresh walk would sit on fresh sets.)
        let mut path = ChunkWaypointSetStatePath::default();
        astar.set_path_for_test(states.clone());
        path.init(&astar);
        assert!(path.matches(&states[1], &states[2]));
        assert!(path.is_current_state(&states[1]));
        assert!(!path.matches(&states[0], &states[2]));

        // A source on the current state is a hit without advancing, and a
        // member state that is neither current nor next refuses — the set
        // path inherits the base's plain isPartOfPath.
        let mut path = ChunkWaypointSetStatePath::default();
        astar.set_path_for_test(states.clone());
        path.init(&astar);
        assert!(path.matches(&states[0], &states[2]));
        assert!(path.is_current_state(&states[0]), "no advance");
        assert!(!path.matches(&states[2], &states[2]), "part-of-path arm");

        // pop on a cleared path is a no-op.
        path.clear();
        path.pop();
        assert!(path.empty());
    }

    #[test]
    fn an_empty_path_is_invalid_and_matches_nothing() {
        let states = chain();
        let mut path = ChunkWaypointSetStatePath::default();
        assert!(path.empty());
        assert!(!path.is_valid());
        assert!(path.p_current().is_none());
        assert!(path.p_next().is_none());
        assert!(path.p_dest().is_none());
        assert!(!path.matches(&states[0], &states[2]));
        path.clear();
        assert!(path.empty());
    }
}
