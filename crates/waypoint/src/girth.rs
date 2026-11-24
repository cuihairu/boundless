//! `lib/waypoint/navigator_find_result.hpp` and the girth selection half of
//! `lib/waypoint/chunk_navigator.cpp` — picking the waypoint set whose
//! girth matches the traveller's, then locating the point in it.
//!
//! `ChunkNavigator::find` filters the cached waypoint sets with
//! `almostEqual(girth)` before searching, which is what makes a narrow
//! doorway reachable for a small girth and not for a wide one.

use super::WaypointIndex;
use super::chunk_waypoint_set_data::ChunkWaypointSetData;
use foundation::math::Vector3;
use std::rc::Rc;

/// `almostEqual(float, float)` (`cstdmf/stdmf.hpp:606-609`): the default
/// `epsilon` of `0.0004f` the girth matching uses.
pub const GIRTH_EPSILON: f32 = 0.0004;

/// `almostEqual` (`cstdmf/stdmf.hpp:606-609`): strict less-than on the
/// absolute difference, so a gap of exactly `epsilon` does not match.
pub fn almost_equal(a: f32, b: f32) -> bool {
    (a - b).abs() < GIRTH_EPSILON
}

/// `NavigatorFindResult` (`navigator_find_result.hpp:21-53`): the set the
/// search landed in, the waypoint it resolved to and whether the search
/// matched exactly.
#[derive(Debug, Clone)]
pub struct NavigatorFindResult {
    /// `pSet_` — the `ChunkWaypointSetPtr`.
    p_set: Option<Rc<ChunkWaypointSetData>>,
    /// `waypoint_`: -1 until the search answers.
    waypoint: WaypointIndex,
    /// `exactMatch_`: false until the search answers.
    exact_match: bool,
}

impl NavigatorFindResult {
    /// `NavigatorFindResult()` (`navigator_find_result.hpp:24-28`): no set,
    /// waypoint -1, not an exact match.
    pub fn new() -> Self {
        Self {
            p_set: None,
            waypoint: -1,
            exact_match: false,
        }
    }

    /// `pSet()` / `pSet(ptr)` (`navigator_find_result.hpp:30-36`).
    pub fn p_set(&self) -> Option<&Rc<ChunkWaypointSetData>> {
        self.p_set.as_ref()
    }

    /// The reference takes the pointer by value and stores it.
    pub fn set_p_set(&mut self, set: Rc<ChunkWaypointSetData>) {
        self.p_set = Some(set);
    }

    /// `waypoint()` / `waypoint(idx)` (`navigator_find_result.hpp:38-44`).
    pub fn waypoint(&self) -> WaypointIndex {
        self.waypoint
    }

    pub fn set_waypoint(&mut self, waypoint: WaypointIndex) {
        self.waypoint = waypoint;
    }

    /// `exactMatch()` / `exactMatch(bool)`
    /// (`navigator_find_result.hpp:46-52`).
    pub fn exact_match(&self) -> bool {
        self.exact_match
    }

    pub fn set_exact_match(&mut self, exact_match: bool) {
        self.exact_match = exact_match;
    }
}

impl Default for NavigatorFindResult {
    /// `new()` is the only construction path: `derive(Default)` would leave
    /// the waypoint at 0, where the reference ctor leaves it at -1.
    fn default() -> Self {
        Self::new()
    }
}

/// The girth-selected search (`chunk_navigator.cpp:135-250`), in the half
/// the port models: the waypoint sets are filtered by girth and the first
/// one with a containing waypoint answers as an exact match; failing that
/// the closest waypoint of every matching set is compared and the nearest
/// answers as a non-exact match.
///
/// Registered boundary: the reference first tries the `GirthGrids`
/// acceleration (a 12x12 grid built per set for outside chunks only, with
/// `findMatchOrLower` plus an ever-widening ring search and the
/// `s_visitedWaypoints_` bookkeeping). That structure is purely a cache of
/// the same answer and needs the chunk's local bounding box to build
/// (BW-10-04-04 landed the clip-based `distanceSquared` it measures with;
/// the grid itself stays out).
pub fn find_by_girth(
    sets: &[Rc<ChunkWaypointSetData>],
    point: Vector3,
    girth: f32,
    ignore_height: bool,
) -> NavigatorFindResult {
    let mut res = NavigatorFindResult::new();

    // First pass: an exact containment in a girth-matching set.
    for set in sets {
        if !almost_equal(set.girth(), girth) {
            continue;
        }
        let found = set.find(point, ignore_height);
        if found >= 0 {
            res.set_p_set(Rc::clone(set));
            res.set_waypoint(found);
            res.set_exact_match(true);
            return res;
        }
    }

    // No exact match, so use the closest one then.
    let mut best_distance_squared = f32::MAX;
    for set in sets {
        if !almost_equal(set.girth(), girth) {
            continue;
        }
        let found = set.find_closest(point, &mut best_distance_squared);
        if found >= 0 {
            res.set_p_set(Rc::clone(set));
            res.set_waypoint(found);
            res.set_exact_match(false);
        }
    }

    res
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse_world_navmesh;

    /// Two girth sets sharing one footprint: the lower square is in both,
    /// the upper square only in the wide one — so a narrow traveller cannot
    /// stand on the upper square and a wide one cannot stand on the lower.
    fn two_layer_sets() -> Vec<Rc<ChunkWaypointSetData>> {
        let mut b = Vec::new();
        let put_f = |b: &mut Vec<u8>, v: f32| b.extend_from_slice(&v.to_le_bytes());
        let put_i = |b: &mut Vec<u8>, v: i32| b.extend_from_slice(&v.to_le_bytes());

        // Set 0 (girth 0.6): the narrow square at height 0..1.
        put_i(&mut b, 0);
        put_f(&mut b, 0.6);
        put_i(&mut b, 1);
        put_i(&mut b, 4);
        put_f(&mut b, 0.0);
        put_f(&mut b, 1.0);
        put_i(&mut b, 4);
        for (x, y) in [(0.0f32, 0.0), (0.0, 2.0), (2.0, 2.0), (2.0, 0.0)] {
            put_f(&mut b, x);
            put_f(&mut b, y);
            put_i(&mut b, 65535);
        }
        // Set 1 (girth 1.0): the wide square at height 2..3 over the same
        // footprint, the narrow doorway above it.
        put_i(&mut b, 0);
        put_f(&mut b, 1.0);
        put_i(&mut b, 1);
        put_i(&mut b, 4);
        put_f(&mut b, 2.0);
        put_f(&mut b, 3.0);
        put_i(&mut b, 4);
        for (x, y) in [(0.0f32, 0.0), (0.0, 2.0), (2.0, 2.0), (2.0, 0.0)] {
            put_f(&mut b, x);
            put_f(&mut b, y);
            put_i(&mut b, 65535);
        }

        let mut sets = parse_world_navmesh(&b).expect("the two sets decode");
        let wide = Rc::new(sets.remove(1));
        let narrow = Rc::new(sets.remove(0));
        vec![narrow, wide]
    }

    #[test]
    fn the_girth_match_window_is_the_reference_epsilon() {
        assert!(almost_equal(0.6, 0.6003));
        // Strict less-than: a gap of exactly the epsilon does not match.
        assert!(!almost_equal(0.6, 0.6004 + 0.0001));
        assert!(!almost_equal(0.6, 1.0));
    }

    #[test]
    fn the_result_starts_empty() {
        let res = NavigatorFindResult::new();
        assert!(res.p_set().is_none());
        assert_eq!(res.waypoint(), -1);
        assert!(!res.exact_match());
        // Default goes through the same construction.
        let res = NavigatorFindResult::default();
        assert_eq!(res.waypoint(), -1);
        let mut res = NavigatorFindResult::new();
        res.set_p_set(Rc::new(ChunkWaypointSetData::new(1)));
        res.set_waypoint(3);
        res.set_exact_match(true);
        assert_eq!(res.waypoint(), 3);
        assert!(res.exact_match());
    }

    #[test]
    fn the_narrow_doorway_is_only_reachable_for_the_narrow_girth() {
        let sets = two_layer_sets();
        assert_eq!(sets[0].girth(), 0.6);
        assert_eq!(sets[1].girth(), 1.0);

        // The narrow traveller finds the lower square...
        let res = find_by_girth(&sets, Vector3::new(1.0, 0.5, 1.0), 0.6, false);
        assert_eq!(res.waypoint(), 0);
        assert!(res.exact_match());
        assert_eq!(res.p_set().unwrap().base_index(), 1);
        // Searching without the height it stands on still answers the layer
        // it projects onto (the best height fit).
        let res = find_by_girth(&sets, Vector3::new(1.0, 2.5, 1.0), 0.6, true);
        assert_eq!(res.waypoint(), 0);
        assert!(res.exact_match());

        // The wide traveller finds the doorway as an exact match...
        let res = find_by_girth(&sets, Vector3::new(1.0, 2.5, 1.0), 1.0, false);
        assert_eq!(res.waypoint(), 0);
        assert!(res.exact_match());
        assert_eq!(res.p_set().unwrap().base_index(), 2);
        // ...but nothing on the lower square, and the fallback finds the
        // doorway as the nearest thing.
        let res = find_by_girth(&sets, Vector3::new(1.0, 0.5, 1.0), 1.0, false);
        assert_eq!(res.waypoint(), 0);
        assert!(!res.exact_match());

        // A point off every footprint: nothing contains it, so the
        // closest-waypoint fallback answers a non-exact match.
        let res = find_by_girth(&sets, Vector3::new(9.0, 0.5, 9.0), 0.6, false);
        assert_eq!(res.waypoint(), 0);
        assert!(!res.exact_match());
        assert_eq!(res.p_set().unwrap().girth(), 0.6);

        // A girth matching neither set answers empty.
        let res = find_by_girth(&sets, Vector3::new(1.0, 0.5, 1.0), 0.3, false);
        assert!(res.p_set().is_none());
        assert_eq!(res.waypoint(), -1);
        assert!(!res.exact_match());
    }

    #[test]
    fn an_empty_set_matching_the_girth_answers_nothing() {
        // A girth-matching set with no waypoints: both passes come back
        // empty, so the result stays unset.
        let mut b = Vec::new();
        let put_f = |b: &mut Vec<u8>, v: f32| b.extend_from_slice(&v.to_le_bytes());
        let put_i = |b: &mut Vec<u8>, v: i32| b.extend_from_slice(&v.to_le_bytes());
        put_i(&mut b, 0);
        put_f(&mut b, 0.6);
        put_i(&mut b, 0);
        put_i(&mut b, 0);

        let mut sets = parse_world_navmesh(&b).expect("the empty set decodes");
        let empty = Rc::new(sets.remove(0));
        assert_eq!(empty.girth(), 0.6);

        let res = find_by_girth(&[empty], Vector3::new(1.0, 0.5, 1.0), 0.6, false);
        assert!(res.p_set().is_none());
        assert_eq!(res.waypoint(), -1);
        assert!(!res.exact_match());
    }
}
