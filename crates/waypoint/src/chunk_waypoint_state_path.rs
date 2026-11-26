//! `lib/waypoint/chunk_waypoint_state_path.{hpp,cpp}` — the per-set
//! waypoint path: the chunk-level A* result reversed into `pop` order,
//! with the full-path cursor the cache nudges as its owner moves.
//!
//! The path-quality face is `smoothPath` (`chunk_waypoint_state_path.cpp:
//! 120-268`): string-pulling the found walk through the shared-edge
//! corridor (the funnel), then rebuilding each kept state through the
//! `NavLoc(guess, point)` constructor — the +0.01 y lift and the
//! same-set re-find of that constructor are the reference's own. On the
//! slope/shell truth world every way path is 2–3 states, which
//! `smoothPath` early-returns from untouched (`reversePath_.size() == 2`);
//! the fixed map's four-state crossing route (`tests/fixed_map.rs`) and
//! the client-query NAV probes (`crates/client/tests/genmap_clientview.rs`)
//! exercise it.

use super::ADJACENT_RANGE_END;
use super::astar::AStar;
use super::chunk_waypoint::ChunkWaypointVertexProvider;
use super::chunk_waypoint_state::ChunkWaypointState;
use super::girth::almost_equal;
use super::navloc::NavLoc;
use super::search_path_base::SearchPathBase;
use foundation::math::{Vector2, Vector3, almost_zero};
use std::rc::Rc;

/// `almostEqual( const Vector3 & )` (`cstdmf/stdmf.hpp:616` family, the
/// 0.0004f default epsilon, per component).
fn almost_equal_vec3(a: Vector3, b: Vector3) -> bool {
    almost_equal(a.x, b.x) && almost_equal(a.y, b.y) && almost_equal(a.z, b.z)
}

/// `almostEqual( const Vector2 & )`: the same scalar epsilon, per
/// component.
fn almost_equal_vec2(a: Vector2, b: Vector2) -> bool {
    almost_equal(a.x, b.x) && almost_equal(a.y, b.y)
}

/// `RelativePosition` (`chunk_waypoint_state_path.hpp:76`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RelativePosition {
    Left,
    Inside,
    Right,
    OnLine,
    BadFunnel,
}

/// The reference's raw `pSet() != pSet()` pointer comparison (null equals
/// null).
fn sets_equal(
    a: Option<&Rc<super::chunk_waypoint_set_data::ChunkWaypointSetData>>,
    b: Option<&Rc<super::chunk_waypoint_set_data::ChunkWaypointSetData>>,
) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => Rc::ptr_eq(a, b),
        (None, None) => true,
        _ => false,
    }
}

/// `NavLoc( const NavLoc & guess, const Vector3 & point )`
/// (`navloc.cpp:97-151`) against a path state's set and grid size: the
/// +0.01 y lift, the same-waypoint projection test, the same-set find
/// fallback.
fn re_located(guess: &NavLoc, point: Vector3) -> NavLoc {
    let set = Rc::clone(guess.p_set().expect("path states are located"));
    NavLoc::from_guess(guess, set, guess.grid_size(), point)
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
        // (the reference's `pSet() == pSet()`: a null pointer equals
        // itself, so setless locations compare by `sets_equal`).
        if s1.nav_loc().waypoint() == -1 || s2.nav_loc().waypoint() == -1 {
            return sets_equal(s1.nav_loc().p_set(), s2.nav_loc().p_set());
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

    /// `smoothPath` (`chunk_waypoint_state_path.cpp:116-268`):
    /// string-pull the walk through the corridor of the edges it crossed
    /// (the funnel), then rebuild the kept states through the
    /// `NavLoc(guess, point)` constructor with the running `maxY` of the
    /// skipped states. Same-waypoint searches and two-state walks answer
    /// untouched; the source and destination states keep their sets and
    /// x/z while every rebuilt point takes the +0.01 y lift of the guess
    /// constructor.
    pub fn smooth_path(
        &mut self,
        astar: &AStar<ChunkWaypointState>,
        src: &ChunkWaypointState,
        dst: &ChunkWaypointState,
    ) -> bool {
        if !self.can_be_smoothed {
            // ERROR_MSG "path cannot be smoothed. It may not have been
            // initialised, or it may have already been smoothed."
            return false;
        }

        // The source and destination points are on the same waypoint, and
        // so smoothing is unnecessary.
        if (src.nav_loc().waypoint() == dst.nav_loc().waypoint()
            && sets_equal(src.nav_loc().p_set(), dst.nav_loc().p_set()))
            || self.reverse_path.len() == 2
        {
            return true;
        }

        // Get the positions of the edge vertices and add them to the
        // corridor, hop by hop (the from set owns the edge's vertices).
        let mut corridor: Vec<(Vector2, Vector2)> = Vec::new();
        let forward = astar.path();
        for hop in forward.windows(2) {
            let from_set = hop[0].nav_loc().p_set();
            let to_set = hop[1].nav_loc().p_set();
            let (from_set, to_set) = match (from_set, to_set) {
                (Some(from), Some(to)) => (from, to),
                // A stateless hop keys nothing the search recorded — the
                // reference's connection lookup answers the sentinel pair.
                _ => return false,
            };

            let (vp1, vp2) = astar.search_data().get_connection(
                from_set,
                hop[0].nav_loc().waypoint(),
                to_set,
                hop[1].nav_loc().waypoint(),
            );
            if vp1 == ADJACENT_RANGE_END || vp2 == ADJACENT_RANGE_END {
                // ERROR_MSG "could not find connecting edge between
                // waypoints %d->%d."
                return false;
            }

            corridor.push((
                *from_set.vertex_by_index(vp1),
                *from_set.vertex_by_index(vp2),
            ));
        }

        if corridor.is_empty() {
            // ERROR_MSG "Path not found."
            return false;
        }

        let src_point = Vector2::new(src.nav_loc().point().x, src.nav_loc().point().z);

        // If the search is not over a chunk boundary, we can use the given
        // destination point.
        let crosses_waypoint_set = !sets_equal(
            forward.last().and_then(|s| s.nav_loc().p_set()),
            forward.first().and_then(|s| s.nav_loc().p_set()),
        );
        let dst_nav_loc = if crosses_waypoint_set {
            dst.nav_loc()
        } else {
            forward.last().expect("corridor is non-empty").nav_loc()
        };
        let dst_point = Vector2::new(dst_nav_loc.point().x, dst_nav_loc.point().z);

        let mut path_points: Vec<(usize, Vector2)> = Vec::new();
        if !funnel(&corridor, src_point, dst_point, &mut path_points) {
            return false;
        }

        let mut new_path: Vec<ChunkWaypointState> =
            vec![ChunkWaypointState::default(); path_points.len() + 2];

        // Add the first element of the original path, the source location
        // of the search.
        let len = new_path.len();
        new_path[len - 1] = self.reverse_path.last().expect("init built ≥2").clone();
        let mut max_y = self
            .reverse_path
            .last()
            .expect("init built ≥2")
            .nav_loc()
            .point()
            .y;

        // Generate the new path from pathPoints. The indices count down
        // from the source end: two back for the path point's edge index
        // (one for the size, one for the extra destination node).
        let mut new_path_index = len - 2;
        let mut prev_reverse_index = self.reverse_path.len() - 2;

        for &(corridor_index, path_point) in &path_points {
            let reverse_path_index = self.reverse_path.len() - corridor_index - 2;

            // Find the maxY of the segment
            while prev_reverse_index > reverse_path_index {
                max_y = max_y.max(self.reverse_path[prev_reverse_index].nav_loc().point().y);
                prev_reverse_index -= 1;
            }

            // Use the navLoc from the old state, but with the new point.
            let nav_loc = self.reverse_path[reverse_path_index].nav_loc();
            let new_point = Vector3::new(path_point.x, max_y.max(nav_loc.point().y), path_point.y);
            let new_state = ChunkWaypointState::from_navloc(&re_located(nav_loc, new_point));
            new_path[new_path_index] = new_state;

            max_y = nav_loc.point().y;
            new_path_index -= 1;
        }

        // Find maxY of last segment
        while prev_reverse_index > 0 {
            max_y = max_y.max(self.reverse_path[prev_reverse_index].nav_loc().point().y);
            prev_reverse_index -= 1;
        }

        // Add the last element of the original path, so that we can assume
        // the end of the path is in the same waypoint as the destination
        // point.
        let nav_loc_last = self.reverse_path[0].nav_loc();
        let path_point_last = Vector3::new(
            nav_loc_last.point().x,
            max_y.max(nav_loc_last.point().y),
            nav_loc_last.point().z,
        );
        new_path[0] = ChunkWaypointState::from_navloc(&re_located(nav_loc_last, path_point_last));

        self.reverse_path = new_path;
        self.can_be_smoothed = false;

        true
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

/// `locationOfPointFromLine` (`chunk_waypoint_state_path.cpp:273-292`):
/// standing on `v1` looking towards `v2`, the side of the line `p` lies
/// on.
fn location_of_point_from_line(v1: Vector2, v2: Vector2, p: Vector2) -> RelativePosition {
    let cross_product = (v2 - v1).cross_product(&(p - v1));

    if almost_zero(cross_product, 0.0004) {
        return RelativePosition::OnLine;
    }

    if cross_product > 0.0 {
        RelativePosition::Left
    } else {
        RelativePosition::Right
    }
}

/// `isPointInsideFunnel` (`chunk_waypoint_state_path.cpp:299-327`): where
/// `query` lies relative to the wedge formed by `root` and the `left` /
/// `right` arms — `BAD_FUNNEL` when the root is on the wrong side of the
/// left-right edge (a reflex funnel).
fn is_point_inside_funnel(
    root: Vector2,
    left: Vector2,
    right: Vector2,
    query: Vector2,
) -> RelativePosition {
    if location_of_point_from_line(left, right, root) == RelativePosition::Left {
        return RelativePosition::BadFunnel;
    }

    if location_of_point_from_line(root, left, query) == RelativePosition::Left {
        return RelativePosition::Left;
    }

    if location_of_point_from_line(root, right, query) == RelativePosition::Right {
        return RelativePosition::Right;
    }

    RelativePosition::Inside
}

/// `processCorridorPoint` (`chunk_waypoint_state_path.cpp:349-447`): test
/// `corridorPoints[current]` against the funnel and progress none or some
/// of the four indices. `false` aborts the smoothing (the unusual-path
/// arm).
fn process_corridor_point(
    corridor_points: &[Vector2],
    root: &mut usize,
    funnel_left: &mut usize,
    funnel_right: &mut usize,
    current: &mut usize,
) -> bool {
    // If one of the funnel arm points is at the same location as the root,
    // we can progress the root to that point. This will allow us to start
    // searching again from further along the corridor.
    if almost_equal_vec2(corridor_points[*root], corridor_points[*funnel_left]) {
        *root = *funnel_left;
        return true;
    }

    if almost_equal_vec2(corridor_points[*root], corridor_points[*funnel_right]) {
        *root = *funnel_right;
        return true;
    }

    // If the point is the same as the previous point on the same side of
    // the corridor, skip it.
    if almost_equal_vec2(corridor_points[*current], corridor_points[*current - 2])
        && *current != corridor_points.len() - 1
    {
        return true;
    }

    let relative_to_funnel = is_point_inside_funnel(
        corridor_points[*root],
        corridor_points[*funnel_left],
        corridor_points[*funnel_right],
        corridor_points[*current],
    );

    let side = if *current % 2 == 1 {
        RelativePosition::Left
    } else {
        RelativePosition::Right
    };

    match relative_to_funnel {
        // The root is on the wrong side of the left-right edge. This can
        // happen when the src position lies on the overlap with another
        // waypoint.
        RelativePosition::BadFunnel => {
            if *root != 0 {
                // INFO_MSG "Failed to smooth unusual path."
                return false;
            }
            *funnel_left += 2;
            *funnel_right += 2;
            *current += 1;
        }

        // `isPointInsideFunnel` never answers ON_LINE — its two line
        // tests compare against LEFT/RIGHT only — so the reference's
        // ON_LINE arm (`:398-411`) is unreachable and is not carried.
        RelativePosition::OnLine => unreachable!("ON_LINE is never returned here"),

        // The point is inside the funnel, so narrow the funnel
        RelativePosition::Inside => {
            if side == RelativePosition::Left {
                *funnel_left = *current;
            } else {
                *funnel_right = *current;
            }
        }

        // The point is behind the current node's wall of the corridor, so
        // ignore this point, unless it is the destination point. The point
        // crossing the funnel closes it off: the stored node for the side
        // getting crossed is the new root.
        crossing @ (RelativePosition::Left | RelativePosition::Right) => {
            if crossing == side {
                if *current == corridor_points.len() - 1 {
                    *root = if side == RelativePosition::Left {
                        *funnel_left
                    } else {
                        *funnel_right
                    };
                }
            } else {
                *root = if side == RelativePosition::Left {
                    *funnel_right
                } else {
                    *funnel_left
                };
            }
        }
    }

    true
}

/// `funnel` (`chunk_waypoint_state_path.cpp:449-581`): the string-pull
/// itself over the corridor points — `[src, edge1.left, edge1.right,
/// edge2.left, ..., dst]` — recording the pulled corner points with the
/// corridor edge they sit on.
fn funnel(
    corridor: &[(Vector2, Vector2)],
    src_pos: Vector2,
    dst_pos: Vector2,
    path_points: &mut Vec<(usize, Vector2)>,
) -> bool {
    let mut corridor_points: Vec<Vector2> = vec![Vector2::ZERO; corridor.len() * 2 + 2];

    corridor_points[0] = src_pos;
    let last = corridor_points.len() - 1;
    corridor_points[last] = dst_pos;

    for (corridor_index, edge) in corridor.iter().enumerate() {
        corridor_points[corridor_index * 2 + 1] = edge.0;
        corridor_points[corridor_index * 2 + 2] = edge.1;
    }

    let mut root: usize = 0;
    let mut prev_root: usize = usize::MAX;

    while prev_root != root {
        prev_root = root;

        // Add the root to the list of path points
        if root != 0 {
            // corridorIndex is the edge index of 'corridor' in which the
            // root point lies.
            let corridor_index = (root - 1) / 2;

            debug_assert!(corridor_index < corridor_points.len());

            let mut should_add_root = true;

            if let Some((prev_index, prev_point)) = path_points.last() {
                // Points should all be from different edges
                debug_assert!(corridor_index != *prev_index);

                // Disregard duplicate points
                if almost_equal_vec2(corridor_points[root], *prev_point) {
                    should_add_root = false;
                }
            }

            // Do not add a point on the last edge, as this will always be
            // explicitly added to the end of the path.
            if corridor_index == corridor.len() - 1 {
                should_add_root = false;
            }

            if should_add_root {
                path_points.push((corridor_index, corridor_points[root]));
            }
        }

        // root is on the left of the corridor, or on the right / the src
        // position.
        let (mut funnel_left, mut funnel_right) = if root % 2 == 1 {
            (root + 2, root + 3)
        } else {
            (root + 1, root + 2)
        };

        // Search until we reach the end or we have a new root.
        let mut current = funnel_right + 1;
        while current < corridor_points.len() && prev_root == root {
            if !process_corridor_point(
                &corridor_points,
                &mut root,
                &mut funnel_left,
                &mut funnel_right,
                &mut current,
            ) {
                return false;
            }
            current += 1;
        }
    }

    true
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

    /// A state at an explicit waypoint and x/z — the funnel tests place
    /// the source and destination on distinct waypoints.
    fn state_at(
        set: &Rc<crate::chunk_waypoint_set_data::ChunkWaypointSetData>,
        waypoint: i32,
        x: f32,
        z: f32,
    ) -> ChunkWaypointState {
        ChunkWaypointState::from_navloc(&NavLoc::from_parts(
            Rc::clone(set),
            waypoint,
            Vector3::new(x, 0.4, z),
            0.0,
        ))
    }

    /// A two-waypoint set whose edge stream plants exactly the four
    /// vertices `(0,0),(4,0),(2,1),(2,-1)` in first-seen order — the
    /// corridor the abort case pulls through.
    fn funnel_set() -> Rc<crate::chunk_waypoint_set_data::ChunkWaypointSetData> {
        let mut b = Vec::new();
        let put_f = |b: &mut Vec<u8>, v: f32| b.extend_from_slice(&v.to_le_bytes());
        let put_i = |b: &mut Vec<u8>, v: i32| b.extend_from_slice(&v.to_le_bytes());
        put_i(&mut b, 0); // version
        put_f(&mut b, 0.6); // girth
        put_i(&mut b, 2); // numWaypoints
        put_i(&mut b, 8); // numEdges
        // wp0 header: heights 0..2, 4 edges.
        put_f(&mut b, 0.0);
        put_f(&mut b, 2.0);
        put_i(&mut b, 4);
        // wp1 header: the same four edges (dedup'd onto the same vertices).
        put_f(&mut b, 0.0);
        put_f(&mut b, 2.0);
        put_i(&mut b, 4);
        for _ in 0..2 {
            put_f(&mut b, 0.0);
            put_f(&mut b, 0.0);
            put_i(&mut b, 65535);
            put_f(&mut b, 4.0);
            put_f(&mut b, 0.0);
            put_i(&mut b, 65535);
            put_f(&mut b, 2.0);
            put_f(&mut b, 1.0);
            put_i(&mut b, 65535);
            put_f(&mut b, 2.0);
            put_f(&mut b, -1.0);
            put_i(&mut b, 65535);
        }
        Rc::new(parse_world_navmesh(&b).expect("decode").remove(0))
    }

    /// A two-waypoint square split along x = 1: wp0 x∈[0,1], wp1 x∈[1,2],
    /// sharing the x=1 edge (the `one_set` polygon convention).
    fn split_square() -> Rc<crate::chunk_waypoint_set_data::ChunkWaypointSetData> {
        let mut b = Vec::new();
        let put_f = |b: &mut Vec<u8>, v: f32| b.extend_from_slice(&v.to_le_bytes());
        let put_i = |b: &mut Vec<u8>, v: i32| b.extend_from_slice(&v.to_le_bytes());
        put_i(&mut b, 0); // version
        put_f(&mut b, 0.6); // girth
        put_i(&mut b, 2); // numWaypoints
        put_i(&mut b, 8); // numEdges
        put_f(&mut b, 0.0); // wp0 heights
        put_f(&mut b, 2.0);
        put_i(&mut b, 4);
        put_f(&mut b, 0.0); // wp1 heights
        put_f(&mut b, 2.0);
        put_i(&mut b, 4);
        for (x, y) in [
            (0.0, 2.0),
            (1.0, 2.0),
            (1.0, 0.0),
            (0.0, 0.0),
            (1.0, 2.0),
            (2.0, 2.0),
            (2.0, 0.0),
            (1.0, 0.0),
        ] {
            put_f(&mut b, x);
            put_f(&mut b, y);
            put_i(&mut b, 65535);
        }
        Rc::new(parse_world_navmesh(&b).expect("decode").remove(0))
    }

    fn v2(x: f32, y: f32) -> Vector2 {
        Vector2::new(x, y)
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

        // Another in-polygon point: the first arm answers again (the
        // projection test is polygon-wide, not point-relative; the
        // full-path walk needs a destination waypoint off the point —
        // `validate_navloc_walks_the_full_path_off_the_next_state`).
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

    #[test]
    fn sets_equal_answers_the_null_and_mixed_cases() {
        let set = one_set();
        let other = one_set();
        // The pointer comparison's null and mixed arms: null equals null,
        // a set never equals null (either way round).
        assert!(sets_equal(None, None));
        assert!(!sets_equal(Some(&set), None));
        assert!(!sets_equal(None, Some(&set)));
        assert!(!sets_equal(Some(&set), Some(&other)), "distinct sets");
        assert!(sets_equal(Some(&set), Some(&set)), "same set");
    }

    #[test]
    fn smooth_path_refuses_an_uninitialised_path() {
        let set = one_set();
        let a = state(&set, 0.1);
        let b = state(&set, 0.7);
        let mut astar = AStar::default();
        astar.set_path_for_test(vec![a.clone(), b.clone()]);
        let mut path = ChunkWaypointStatePath::default();
        // Never initialised: canBeSmoothed_ is false.
        assert!(!path.smooth_path(&astar, &a, &b));
    }

    #[test]
    fn smooth_path_refuses_a_stateless_hop() {
        let set = funnel_set();
        let a = state_at(&set, 0, 0.1, 0.5);
        let mid = ChunkWaypointState::from_navloc(&NavLoc::new());
        let b = state_at(&set, 1, 0.7, 0.5);
        let mut astar = AStar::default();
        astar.set_path_for_test(vec![a.clone(), mid, b.clone()]);
        let mut path = ChunkWaypointStatePath::default();
        path.init(&astar);
        // The stateless middle hop keys nothing the search recorded — the
        // connection lookup answers the sentinel pair and smoothing aborts.
        assert!(!path.smooth_path(&astar, &a, &b));
    }

    #[test]
    fn smooth_path_refuses_an_empty_walk() {
        let set = funnel_set();
        // Distinct waypoints keep the same-waypoint early return out of the
        // way; the corridor is what's empty.
        let a = state_at(&set, 0, 0.1, 0.5);
        let b = state_at(&set, 1, 0.7, 0.5);
        let mut astar = AStar::default();
        astar.set_path_for_test(vec![]);
        let mut path = ChunkWaypointStatePath::default();
        path.init(&astar);
        // `astar.path()` — the corridor source — is empty, so the corridor
        // is empty.
        assert!(!path.smooth_path(&astar, &a, &b));
    }

    #[test]
    fn smooth_path_aborts_when_the_root_is_behind_the_wall() {
        let set = funnel_set();
        // The corridor the search recorded: wp0->wp1 crosses the (0,0)-(4,0)
        // edge (edge indices 0,1), then wp1->wp1 crosses (2,1)-(2,-1)
        // (edge indices 2,3).
        let src = state_at(&set, 0, -2.0, -2.0);
        let mid = state_at(&set, 1, 0.0, -2.0);
        let dst = state_at(&set, 1, 0.0, -2.0);
        let mut astar = AStar::default();
        astar.set_path_for_test(vec![src.clone(), mid, dst.clone()]);
        astar
            .search_data_mut()
            .set_connection(Some(&set), 0, Some(&set), 1, 0, 1);
        astar
            .search_data_mut()
            .set_connection(Some(&set), 1, Some(&set), 1, 2, 3);
        let mut path = ChunkWaypointStatePath::default();
        path.init(&astar);
        // The funnel pulls the corner (4,0) onto the path, then the next
        // corridor point lands behind the funnel — the reference's
        // "Failed to smooth unusual path."
        assert!(!path.smooth_path(&astar, &src, &dst));
    }

    #[test]
    fn smooth_path_pulls_the_corner_through_the_funnel() {
        let set = funnel_set();
        // A duplicated corridor edge: the funnel pulls (0,0) onto the path
        // (a push), then the second edge's corner is a duplicate point
        // (disregarded) and the root's own edge is the last edge (not
        // added again).
        let src = state_at(&set, 0, -2.0, -2.0);
        let mid = state_at(&set, 1, -2.0, 0.0);
        let dst = state_at(&set, 1, -2.0, 0.0);
        let mut astar = AStar::default();
        astar.set_path_for_test(vec![src.clone(), mid, dst.clone()]);
        astar
            .search_data_mut()
            .set_connection(Some(&set), 0, Some(&set), 1, 0, 1);
        astar
            .search_data_mut()
            .set_connection(Some(&set), 1, Some(&set), 1, 0, 1);
        let mut path = ChunkWaypointStatePath::default();
        path.init(&astar);
        assert!(path.smooth_path(&astar, &src, &dst));
        // The pulled corner (0,0) is the middle state's new point (the
        // source keeps its own, the destination its own).
        let mid_point = path.reverse_path()[1].nav_loc().point();
        assert!((mid_point.x - 0.0).abs() <= 1e-6, "x = 0");
        assert!((mid_point.z - 0.0).abs() <= 1e-6, "z = 0");
        assert!((mid_point.y - 0.41).abs() <= 1e-5, "the +0.01 lift");
        assert!(!path.can_be_smoothed(), "the flag is cleared");
    }

    #[test]
    fn the_funnel_arms_track_the_root_position() {
        // ROOT on the left: the root advances to the funnel's left arm,
        // then the corner is pulled onto the path. The duplicated edge
        // keeps the first edge off the last, so the push happens.
        let edge = (v2(0.0, 0.0), v2(4.0, 0.0));
        let mut pts = Vec::new();
        assert!(funnel(
            &[edge, edge],
            v2(0.0, 0.0),
            v2(-2.0, -2.0),
            &mut pts
        ));
        assert_eq!(pts, vec![(0, v2(0.0, 0.0))]);

        // ROOT on the right: the root advances to the funnel's right arm.
        let mut pts = Vec::new();
        assert!(funnel(
            &[edge, edge],
            v2(-2.0, -2.0),
            v2(0.0, -2.0),
            &mut pts
        ));
        assert_eq!(pts, vec![(0, v2(4.0, 0.0))]);
    }

    #[test]
    fn the_funnel_resets_a_bad_root_at_the_source() {
        // The source lies on the overlap: the first corridor point is
        // behind the funnel, but the root is still the source, so the
        // funnel restarts two edges further on.
        let mut pts = Vec::new();
        assert!(funnel(
            &[(v2(0.0, 0.0), v2(4.0, 0.0))],
            v2(-2.0, 2.0),
            v2(-2.0, -2.0),
            &mut pts,
        ));
        assert!(pts.is_empty(), "no corner is pulled");
    }

    #[test]
    fn the_funnel_closes_at_the_destination_point() {
        // The destination crosses the funnel's open side: the root closes
        // onto the funnel's stored arm for that side.
        let mut pts = Vec::new();
        assert!(funnel(
            &[(v2(0.0, 0.0), v2(4.0, 0.0))],
            v2(-2.0, -2.0),
            v2(-2.0, 0.0),
            &mut pts,
        ));
        assert!(pts.is_empty(), "the corner is the destination's edge");
    }

    #[test]
    fn validate_navloc_walks_the_full_path_off_the_next_state() {
        let set = split_square();
        // A path a -> b: the next state (b) is on wp1, so a probe inside
        // wp0 answers the full-path walk, not the next-state arm.
        let a = state_at(&set, 0, 0.5, 0.5);
        let b = state_at(&set, 1, 1.5, 0.5);
        let mut path = ChunkWaypointStatePath::default();
        path.init(&{
            let mut astar = AStar::default();
            astar.set_path_for_test(vec![a.clone(), b.clone()]);
            astar
        });

        // Inside wp0's polygon, off the next state's waypoint: the walk
        // reaches the full path's source state.
        let located = path
            .validate_navloc(Vector3::new(0.5, 0.4, 0.5))
            .expect("hit the source state");
        assert_eq!(located.waypoint(), 0);
        assert!((located.point().x - 0.5).abs() <= 1e-5);
        assert!((located.point().y - 0.41).abs() <= 1e-5, "the +0.01 lift");
    }
}
