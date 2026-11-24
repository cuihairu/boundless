//! `lib/waypoint/navigator.{hpp,cpp}` — the two-level search orchestrator:
//! the waypoint-set path across chunks (`searchNextWaypointSet`), then the
//! in-set waypoint path (`searchNextLocalWaypoint`), glued by
//! `searchNextWaypoint` and exposed through `findPath`.
//!
//! The reference's `findPath` returning false **is** the unreachable
//! result: a set-level miss or an in-set miss (a closed portal —
//! `blockNonPermissive` over a non-permissive connection — is one) leaves
//! `nextWaypoint` untouched and answers false.
//!
//! Registered boundaries:
//! - `searchNextLocalWaypoint`'s cursor nudge reaches through `pCache_`
//!   (the member cache) rather than the `cache` parameter the rest of the
//!   method uses; the port nudges the parameter, observably identical
//!   through `findPath` (see the method's doc).
//! - `findFullPath`/`createPathNavLoc` need the `NavLoc(pSpace, …)` and
//!   `NavLoc(guess with waypoint -1)` space faces (registered-deferred
//!   since 04-02) and land with the chunk space lane; `dumpCache` is a
//!   debug print.
//! - the reference mutates `pCache_` through a pointer from `const`
//!   methods; the port holds it in a `RefCell`.

use super::astar::AStar;
use super::chunk_waypoint_set_state::ChunkWPSetState;
use super::chunk_waypoint_state::ChunkWaypointState;
use super::navigator_cache::NavigatorCache;
use super::navloc::NavLoc;
use super::search_path_base::SearchPathBase;
use foundation::math::Vector3;
use std::cell::RefCell;
use std::rc::Rc;

/// `Navigator::CacheState` (`navigator.hpp:37-41`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CacheState {
    /// `READ_CACHE`.
    ReadCache,
    /// `READ_WRITE_CACHE`.
    ReadWriteCache,
}

/// `Navigator` (`navigator.hpp:31-88`).
#[derive(Debug, Default)]
pub struct Navigator {
    /// `pCache_`.
    cache: RefCell<NavigatorCache>,
}

fn same_set(a: &NavLoc, b: &NavLoc) -> bool {
    match (a.p_set(), b.p_set()) {
        (Some(a), Some(b)) => Rc::ptr_eq(a, b),
        (None, None) => true,
        _ => false,
    }
}

impl Navigator {
    /// `Navigator::findPath` (`navigator.cpp:118-159`): the cached face.
    /// On success `next_waypoint` is the next waypoint along — the
    /// destination itself when the cached next state is the destination
    /// ("Use dst as point might not match"); it may only be semi-valid
    /// (the caller re-locates it through the `guess` constructor).
    #[allow(clippy::too_many_arguments)] // the reference's in/out list
    pub fn find_path(
        &self,
        src: &NavLoc,
        dst: &NavLoc,
        max_search_distance: f32,
        block_non_permissive: bool,
        next_waypoint: &mut NavLoc,
        passed_activated_portal: &mut bool,
        cache_state: CacheState,
    ) -> bool {
        // When the cache is set for Read only, we need to verify the path
        // matches the existing path destination and source, if so we can
        // continue using the stored cache in pCache_.
        if cache_state == CacheState::ReadCache
            && !self.cache.borrow().way_path().empty()
            && (!self
                .cache
                .borrow()
                .way_path()
                .is_current_state(&ChunkWaypointState::from_navloc(src))
                || !self
                    .cache
                    .borrow()
                    .way_path()
                    .is_destination_state(&ChunkWaypointState::from_navloc(dst)))
        {
            let mut cache = NavigatorCache::default();
            if self.search_next_waypoint(
                &mut cache,
                src,
                dst,
                max_search_distance,
                block_non_permissive,
                Some(passed_activated_portal),
            ) {
                *next_waypoint = cache
                    .p_next_waypoint_state()
                    .expect("valid path")
                    .nav_loc()
                    .clone();
                return true;
            }
            return false;
        }

        // Using cache, or cache matches use cache (won't be overwritten).
        if self.search_next_waypoint(
            &mut self.cache.borrow_mut(),
            src,
            dst,
            max_search_distance,
            block_non_permissive,
            Some(passed_activated_portal),
        ) {
            let next = self
                .cache
                .borrow()
                .p_next_waypoint_state()
                .expect("valid path")
                .nav_loc()
                .clone();
            if next.waypoints_equal(dst) {
                // Use dst as point might not match
                *next_waypoint = dst.clone();
            } else {
                *next_waypoint = next;
            }
            return true;
        }

        false
    }

    /// `Navigator::searchNextWaypoint` (`navigator.cpp:411-481`).
    pub fn search_next_waypoint(
        &self,
        cache: &mut NavigatorCache,
        src: &NavLoc,
        dst: &NavLoc,
        max_search_distance: f32,
        block_non_permissive: bool,
        passed_activated_portal: Option<&mut bool>,
    ) -> bool {
        assert!(
            src.valid() && dst.valid(),
            "reference MF_ASSERT_DEBUGs the endpoints are valid"
        );

        // The reference clears `*pPassedActivatedPortal` on entry and may
        // raise it on the way out; both writes go through the caller's
        // flag, or nowhere when the caller handed none.
        let mut dropped = false;
        let activated_portal = match passed_activated_portal {
            Some(flag) => flag,
            None => &mut dropped,
        };
        *activated_portal = false;

        let src_state = ChunkWaypointState::from_navloc(src);
        let mut dst_state = ChunkWaypointState::from_navloc(dst);

        let mut next_set_state: Option<ChunkWPSetState> = None;

        if !same_set(src, dst) {
            // The source and destination are in different waypoint sets.
            // We need to find a path amongst the waypoint sets.
            let mut src_set_state = ChunkWPSetState::from_navloc(src);
            let dst_set_state = ChunkWPSetState::from_navloc(dst);
            src_set_state.set_block_non_permissive(block_non_permissive);

            if !self.search_next_waypoint_set(
                cache,
                &src_set_state,
                &dst_set_state,
                max_search_distance,
            ) {
                return false;
            }

            let set_state = cache
                .p_next_waypoint_set_state()
                .cloned()
                .expect("the reference MF_ASSERTs the next set state");
            next_set_state = Some(set_state.clone());

            // Search to the edge of the current waypoint set.
            dst_state = ChunkWaypointState::for_destination(
                Rc::clone(set_state.p_set().unwrap()),
                dst.point(),
            );
        }

        // It makes no sense to check for maxSearchDistance inside navpoly
        // set if it is greater than gridSize.
        let max_search_distance_in_set = if max_search_distance > src.grid_size() {
            -1.0
        } else {
            max_search_distance
        };

        if !self.search_next_local_waypoint(
            cache,
            &src_state,
            &dst_state,
            max_search_distance_in_set,
        ) {
            return false;
        }

        // If we traversed a set, see if we went through an activated
        // portal.
        if next_set_state.is_some()
            && !same_set(
                cache.p_next_waypoint_state().expect("valid path").nav_loc(),
                src,
            )
            && next_set_state.unwrap().passed_activated_portal()
        {
            *activated_portal = true;
        }

        true
    }

    /// `Navigator::searchNextLocalWaypoint` (`navigator.cpp:499-527`).
    /// The reference's cursor nudge at `navigator.cpp:508` reaches through
    /// `pCache_` (the member) where the method's other cache reads use the
    /// parameter. Through `findPath` the parameter **is** the member in the
    /// only arm that can reach the nudge — the read-only scratch cache
    /// starts empty and never matches — so nudging the parameter is
    /// observably identical (registered; a direct caller handing a
    /// pre-filled scratch cache would see the reference nudge its member
    /// instead).
    pub fn search_next_local_waypoint(
        &self,
        cache: &mut NavigatorCache,
        src_state: &ChunkWaypointState,
        dst_state: &ChunkWaypointState,
        max_search_distance: f32,
    ) -> bool {
        if cache.find_way_path(src_state, dst_state) {
            // Remove potential overlapping source states
            cache.move_state_along_path(src_state);
        } else {
            // Search, and reset the cache.
            let mut astar = AStar::default();
            if astar.search(src_state, dst_state, max_search_distance) {
                cache.save_way_path(&astar, src_state, dst_state);
            } else {
                cache.clear_way_path(); // for sanity
                return false;
            }
        }
        true
    }

    /// `Navigator::searchNextWaypointSet` (`navigator.cpp:538-571`).
    pub fn search_next_waypoint_set(
        &self,
        cache: &mut NavigatorCache,
        src_state: &ChunkWPSetState,
        dst_state: &ChunkWPSetState,
        max_search_distance: f32,
    ) -> bool {
        // Don't use the cache if the cache contains a waypoint set path
        // that passed through a portal belonging to an indoor chunk.
        let bypass_cache = cache.way_set_path_passed_shell_boundary();

        if bypass_cache || !cache.find_way_set_path(src_state, dst_state) {
            // Recalculate the waypoint set path via A* search.
            cache.clear_way_path();
            cache.clear_way_set_path();

            let mut astar_set = AStar::default();
            if astar_set.search(src_state, dst_state, max_search_distance) {
                cache.save_way_set_path(&astar_set);
            } else {
                cache.clear_way_set_path(); // for sanity
                return false;
            }
        }

        true
    }

    /// `Navigator::getCachedWaypointPath` (`navigator.cpp:356-362`) with
    /// the anonymous `addWaypointPathPoints` (`navigator.cpp:34-47`): the
    /// intermediate points plus the destination — the current state is
    /// not part of the answer.
    pub fn get_cached_waypoint_path(&self) -> Vec<Vector3> {
        let cache = self.cache.borrow();
        let path = cache.way_path();
        (0..path.reverse_path().len().saturating_sub(1))
            .rev()
            .map(|index| path.reverse_path()[index].nav_loc().point())
            .collect()
    }

    /// `Navigator::getWaySetPathSize` (`navigator.cpp:368-371`).
    pub fn get_way_set_path_size(&self) -> usize {
        self.cache.borrow().get_way_set_path_size()
    }

    /// `Navigator::clearCachedWayPath` (`navigator.cpp:377-380`).
    pub fn clear_cached_way_path(&self) {
        self.cache.borrow_mut().clear_way_path();
    }

    /// `Navigator::clearCachedWaySetPath` (`navigator.cpp:386-389`).
    pub fn clear_cached_way_set_path(&self) {
        self.cache.borrow_mut().clear_way_set_path();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chunk_waypoint_set::Portal;
    use crate::chunk_waypoint_set_data::ChunkWaypointSetData;

    /// One single-waypoint square set per call.
    fn one_set() -> Rc<ChunkWaypointSetData> {
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

    /// Two sets with one permissive portal connection A → B. The sets need
    /// chunk identities: the cross-set arm refuses a neighbour whose set
    /// was never mounted on a chunk.
    fn wired_pair() -> (Rc<ChunkWaypointSetData>, Rc<ChunkWaypointSetData>) {
        let a = one_set();
        let b = one_set();
        a.set_chunk_identity(0);
        b.set_chunk_identity(1);
        let portal = Rc::new(Portal {
            permissive: true,
            has_chunk_item: false,
        });
        // Both directions, as the reference's bind() wiring does: a set
        // only counts as a neighbour when it can get back.
        a.connect(b.clone(), portal.clone(), 2, 0, Vector3::new(0.5, 0.5, 0.5));
        b.connect(a.clone(), portal, 0, 0, Vector3::new(0.5, 0.5, 0.5));
        (a, b)
    }

    fn at(set: &Rc<ChunkWaypointSetData>, x: f32) -> NavLoc {
        NavLoc::from_parts(Rc::clone(set), 0, Vector3::new(x, 0.4, 0.5), 0.0)
    }

    #[test]
    fn the_write_read_path_fills_the_cache_and_reports_its_size() {
        let (a, b) = wired_pair();
        let navigator = Navigator::default();
        let src = at(&a, 0.2);
        let dst = at(&b, 0.8);
        let mut next = NavLoc::new();
        let mut act = false;

        assert!(navigator.find_path(
            &src,
            &dst,
            1_000_000.0,
            false,
            &mut next,
            &mut act,
            CacheState::ReadWriteCache
        ));
        assert!(Rc::ptr_eq(next.p_set().unwrap(), &b));
        assert_eq!(next.waypoint(), 0);
        assert!(!act, "an unactivated portal sets no flag");
        assert_eq!(navigator.get_way_set_path_size(), 2, "A then B");
        assert_eq!(navigator.get_cached_waypoint_path().len(), 1);

        navigator.clear_cached_way_path();
        assert!(navigator.cache.borrow().way_path().empty());
        navigator.clear_cached_way_set_path();
        assert_eq!(navigator.get_way_set_path_size(), 0);
    }

    #[test]
    fn a_read_cache_ignores_a_mismatching_cached_path() {
        let (a, b) = wired_pair();
        let navigator = Navigator::default();
        let mut next = NavLoc::new();
        let mut act = false;

        // Prime the member cache with A → B.
        assert!(navigator.find_path(
            &at(&a, 0.2),
            &at(&b, 0.8),
            1_000_000.0,
            false,
            &mut next,
            &mut act,
            CacheState::ReadWriteCache
        ));
        let cached = navigator.get_cached_waypoint_path();

        // A read-only pass from B answers through a scratch cache, so the
        // member cache is left as it was.
        assert!(navigator.find_path(
            &at(&b, 0.5),
            &at(&a, 0.3),
            1_000_000.0,
            false,
            &mut next,
            &mut act,
            CacheState::ReadCache
        ));
        assert!(Rc::ptr_eq(next.p_set().unwrap(), &a));
        assert_eq!(navigator.get_cached_waypoint_path(), cached);

        // An empty cache has nothing to verify and falls through to the
        // write path.
        let fresh = Navigator::default();
        assert!(fresh.find_path(
            &at(&a, 0.2),
            &at(&b, 0.8),
            1_000_000.0,
            false,
            &mut next,
            &mut act,
            CacheState::ReadCache
        ));
        assert_eq!(fresh.get_way_set_path_size(), 2);
    }

    #[test]
    fn an_unreachable_destination_answers_false_and_untouches_the_next() {
        let a = one_set();
        let c = one_set(); // no connection to `a` at all
        let navigator = Navigator::default();
        let mut next = NavLoc::new();
        let mut act = true;

        assert!(!navigator.find_path(
            &at(&a, 0.2),
            &at(&c, 0.8),
            1_000_000.0,
            false,
            &mut next,
            &mut act,
            CacheState::ReadWriteCache
        ));
        assert!(!next.valid(), "the next stays invalid");
        assert!(!act, "the flag is cleared on the way out");
        assert_eq!(navigator.get_way_set_path_size(), 0, "cleared for sanity");
    }

    #[test]
    fn a_repeat_call_rides_the_caches() {
        let (a, b) = wired_pair();
        // Both chunks outside: the hop crosses no shell boundary, so the
        // saved set path is reusable instead of bypassed.
        a.set_outside_chunk(true);
        b.set_outside_chunk(true);
        let navigator = Navigator::default();
        let mut next = NavLoc::new();
        let mut act = false;

        assert!(navigator.find_path(
            &at(&a, 0.2),
            &at(&b, 0.8),
            1_000_000.0,
            false,
            &mut next,
            &mut act,
            CacheState::ReadWriteCache
        ));

        // The exact repeat hits both caches: the set path through the
        // current state, the way path through the current state — and the
        // member cache's cursor still nudges forward.
        assert!(navigator.find_path(
            &at(&a, 0.2),
            &at(&b, 0.8),
            1_000_000.0,
            false,
            &mut next,
            &mut act,
            CacheState::ReadWriteCache
        ));
        assert!(Rc::ptr_eq(next.p_set().unwrap(), &b));
    }

    #[test]
    fn a_read_only_call_with_a_new_destination_uses_a_scratch_cache() {
        let (a, b) = wired_pair();
        let c = one_set(); // mounted, but no connection to either
        c.set_chunk_identity(2);
        let navigator = Navigator::default();
        let mut next = NavLoc::new();
        let mut act = false;

        assert!(navigator.find_path(
            &at(&a, 0.2),
            &at(&b, 0.8),
            1_000_000.0,
            false,
            &mut next,
            &mut act,
            CacheState::ReadWriteCache
        ));
        let cached = navigator.get_cached_waypoint_path();

        // The read-only pass sees the current state still matches but the
        // destination does not, so it searches a scratch cache — and the
        // unreachable destination leaves the member cache as it was.
        assert!(!navigator.find_path(
            &at(&a, 0.2),
            &at(&c, 0.8),
            1_000_000.0,
            false,
            &mut next,
            &mut act,
            CacheState::ReadCache
        ));
        assert!(
            Rc::ptr_eq(next.p_set().unwrap(), &b),
            "the untouched next keeps the previous answer"
        );
        assert_eq!(navigator.get_cached_waypoint_path(), cached);
        assert_eq!(navigator.get_way_set_path_size(), 2);
    }

    #[test]
    fn the_none_flag_and_the_in_set_distance_clamp_arms_run() {
        let (a, b) = wired_pair();
        let navigator = Navigator::default();
        let mut cache = NavigatorCache::default();

        // No flag to write through, and a max search distance at or under
        // the grid size passes through unfiltered.
        assert!(navigator.search_next_waypoint(
            &mut cache,
            &at(&a, 0.2),
            &at(&b, 0.8),
            -1.0,
            false,
            None
        ));
        assert_eq!(cache.get_way_set_path_size(), 2);

        // Above the grid size the in-set search is clamped to unlimited —
        // "it makes no sense to check for maxSearchDistance inside navpoly
        // set if it is greater than gridSize" — so a limit far past the
        // crossing still answers.
        assert!(navigator.search_next_waypoint(
            &mut cache,
            &at(&a, 0.2),
            &at(&b, 0.8),
            1000.0,
            false,
            None
        ));

        // A limit tight under the pair distance fails outright; the
        // shell-boundary bypass re-searches, so the limit is what loses.
        assert!(!navigator.search_next_waypoint(
            &mut cache,
            &at(&a, 0.2),
            &at(&b, 0.8),
            0.1,
            false,
            None
        ));
    }

    #[test]
    fn an_activated_portal_raises_the_flag() {
        let a = one_set();
        let b = one_set();
        a.set_chunk_identity(0);
        b.set_chunk_identity(1);
        // Different chunk identities with both chunks inside: the hop
        // crosses a shell boundary, and the item-carrying portal is
        // activated.
        let portal = Rc::new(Portal {
            permissive: true,
            has_chunk_item: true,
        });
        a.connect(b.clone(), portal.clone(), 2, 0, Vector3::new(0.5, 0.5, 0.5));
        b.connect(a.clone(), portal, 0, 0, Vector3::new(0.5, 0.5, 0.5));

        let navigator = Navigator::default();
        let mut next = NavLoc::new();
        let mut act = false;
        assert!(navigator.find_path(
            &at(&a, 0.2),
            &at(&b, 0.8),
            1_000_000.0,
            false,
            &mut next,
            &mut act,
            CacheState::ReadWriteCache
        ));
        assert!(act, "the activated portal raised the flag");
    }

    #[test]
    fn a_local_level_miss_answers_false() {
        let (a, _) = wired_pair();
        let c = one_set();
        let navigator = Navigator::default();
        let mut cache = NavigatorCache::default();

        // The goal set is not reachable from the source set at the local
        // level, so the in-set search misses and the cache clears.
        let src_state = ChunkWaypointState::from_navloc(&at(&a, 0.2));
        let dst_state =
            ChunkWaypointState::for_destination(Rc::clone(&c), Vector3::new(0.8, 0.4, 0.5));
        assert!(!navigator.search_next_local_waypoint(&mut cache, &src_state, &dst_state, -1.0));
        assert!(!cache.way_path().is_valid());
    }

    #[test]
    fn the_setless_same_set_arms_answer() {
        let (a, _) = wired_pair();
        let setless = NavLoc::new();
        assert!(same_set(&setless, &NavLoc::new()), "both setless");
        assert!(!same_set(&at(&a, 0.2), &setless), "one setless");
    }
}
