//! `lib/waypoint/navloc.{hpp,cpp}` — a location in the navigation mesh:
//! the waypoint set plus the waypoint index it resolved to.
//!
//! The reference's set pointer is the `ChunkWaypointSetPtr` (the chunk
//! wrapper over `ChunkWaypointSetData`); the port holds the decoded set
//! data directly, so the chunk mounting and the space/chunk lookup
//! constructors stay out of this face (the chunk lane's). The
//! waypoint-space coordinates are the identity here.

use super::WaypointIndex;
use super::chunk_waypoint::ChunkWaypointVertexProvider;
use super::chunk_waypoint_set_data::ChunkWaypointSetData;
use foundation::math::Vector3;
use std::rc::Rc;

/// `NavLoc` (`navloc.hpp:22-73`): the location plus the waypoint it is
/// located on. The default result is always invalid.
#[derive(Debug, Clone, Default)]
pub struct NavLoc {
    /// `pSet_` — the `ChunkWaypointSetPtr`; an `Rc` keeps the reference
    /// count and identity semantics the pointer comparison relies on.
    p_set: Option<Rc<ChunkWaypointSetData>>,
    /// `waypoint_`: the index into `pSet_`, or -1 when the search missed.
    waypoint: WaypointIndex,
    /// `point_`: the location in waypoint-space coordinates.
    point: Vector3,
    /// `gridSize_`.
    grid_size: f32,
}

impl NavLoc {
    /// `NavLoc()` (`navloc.cpp:19-25`): no set, waypoint 0.
    pub fn new() -> Self {
        Self::default()
    }

    /// The `NavLoc(const NavLoc & guess, const Vector3 & point)`
    /// constructor (`navloc.cpp:97-151`), in the part the waypoint crate
    /// models: the same waypoint wins when the point projects into it,
    /// otherwise the guess's own set search answers. `set` plays the
    /// guess's set pointer and `grid_size` its space's grid size.
    ///
    /// Registered boundary: the reference falls back to a full space search
    /// (`NavLoc(pSet->chunk()->space(), point_, girth())`) when the set
    /// search misses, and routes waypoint -1 guesses through
    /// `pSpace->collide`; that needs the chunk/space faces and lands in
    /// the search lane (BW-10-04-04/05). Waypoint -1 stays -1 here.
    pub fn from_guess(
        guess: &NavLoc,
        set: Rc<ChunkWaypointSetData>,
        grid_size: f32,
        point: Vector3,
    ) -> Self {
        assert!(
            guess.valid(),
            "reference MF_ASSERT_DEBUGs the guess is valid"
        );

        let mut point = point;
        point.y += 0.01f32;

        let waypoint = if guess.contains_projection(point) {
            guess.waypoint
        } else {
            set.find(point, false)
        };

        Self {
            p_set: Some(Rc::clone(&set)),
            waypoint,
            point,
            grid_size,
        }
    }

    /// Direct construction from a resolved set/waypoint/point — the part
    /// `NavLoc`'s private fields play in `ChunkWaypointState`'s
    /// constructors and `ChunkWaypointState::getAdjacency`, which the
    /// reference reaches through its `friend class ChunkWaypointState`
    /// clause. Waypoint -1 with a set is the goal-only state
    /// (`ChunkWaypointState(dstSet, dstPoint)`). `grid_size` is the space's
    /// grid size the located reference constructors store
    /// (`gridSize_( pChunk->space()->gridSize() )`, `navloc.cpp:66-70`) —
    /// the value `searchNextWaypoint`'s in-set clamp reads. Public so the
    /// replay harness can rebuild the reference probes' already-resolved
    /// locations; the find-based `from_guess` stays the primary face.
    pub fn from_parts(
        set: Rc<ChunkWaypointSetData>,
        waypoint: WaypointIndex,
        point: Vector3,
        grid_size: f32,
    ) -> Self {
        Self {
            p_set: Some(set),
            waypoint,
            point,
            grid_size,
        }
    }

    /// `NavLoc::clip()` (`navloc.cpp:183-215`): the public face the
    /// navigation system calls — `createPathNavLoc` and the navigation
    /// controller's destination clip. The stored point is pulled into
    /// the located waypoint.
    pub fn clip(&mut self) {
        self.clip_in_place();
    }

    /// The stored point is pulled into the located waypoint. The chunk
    /// mapping is the identity here (registered).
    pub(crate) fn clip_in_place(&mut self) {
        if let Some(set) = &self.p_set
            && self.waypoint >= 0
        {
            let wp = &set.waypoints()[self.waypoint as usize];
            wp.clip(set.edges(), set.as_ref(), &mut self.point);
        }
    }

    /// `valid()` (`navloc.hpp:35-36`): the set is present. The reference's
    /// `pSet_->chunk()` test needs the chunk mounting and is registered.
    pub fn valid(&self) -> bool {
        self.p_set.is_some()
    }

    /// `pSet()` (`navloc.hpp:38-40`).
    pub fn p_set(&self) -> Option<&Rc<ChunkWaypointSetData>> {
        self.p_set.as_ref()
    }

    /// `waypoint()` (`navloc.hpp:41-44`).
    pub fn waypoint(&self) -> WaypointIndex {
        self.waypoint
    }

    /// `point()` (`navloc.hpp:46-49`).
    pub fn point(&self) -> Vector3 {
        self.point
    }

    /// `gridSize()` (`navloc.hpp:50-52`).
    pub fn grid_size(&self) -> f32 {
        self.grid_size
    }

    /// `isWithinWP()` (`navloc.cpp:165-177`): the stored point is still
    /// inside the located waypoint.
    pub fn is_within_wp(&self) -> bool {
        if let Some(set) = &self.p_set
            && self.waypoint >= 0
        {
            return set.waypoints()[self.waypoint as usize].contains(
                set.edges(),
                set.as_ref(),
                self.point,
            );
        }
        false
    }

    /// `makeMaxHeight` (`navloc.cpp:193-201`): the point is raised to the
    /// waypoint's `maxHeight_`. The reference maps it through
    /// `WorldSpaceVector3`; the port's waypoint space is the identity
    /// mapping (registered).
    pub fn make_max_height(&self, point: &mut Vector3) {
        if let Some(set) = &self.p_set
            && self.waypoint >= 0
        {
            set.waypoints()[self.waypoint as usize].make_max_height(point);
        }
    }

    /// `waypointsEqual` (`navloc.hpp:60-61`): the same set object and the
    /// same waypoint index.
    pub fn waypoints_equal(&self, other: &NavLoc) -> bool {
        match (&self.p_set, &other.p_set) {
            (Some(a), Some(b)) => Rc::ptr_eq(a, b) && self.waypoint == other.waypoint,
            (None, None) => self.waypoint == other.waypoint,
            _ => false,
        }
    }

    /// `containsProjection` (`navloc.cpp:221-236`): near-coincident points
    /// answer true straight away (the `0.00001f` window), otherwise the
    /// waypoint's projection test decides.
    pub fn contains_projection(&self, point: Vector3) -> bool {
        if self.p_set.is_none() || self.waypoint < 0 {
            return false;
        }
        let dx = self.point.x - point.x;
        let dy = self.point.y - point.y;
        let dz = self.point.z - point.z;
        if dx * dx + dy * dy + dz * dz < 0.00001 {
            return true;
        }

        let set = self.p_set.as_ref().unwrap();
        set.waypoints()[self.waypoint as usize].contains_projection(
            set.edges(),
            set.as_ref(),
            point,
        )
    }

    /// `desc()` (`navloc.cpp:242-279`): the set id, the waypoint, the
    /// point and the waypoint's vertices. The reference prefixes the chunk
    /// identifier and the raw set pointer; the port reports the set's
    /// `baseIndex_` in the identifier's place and omits the pointer
    /// (registered).
    pub fn desc(&self) -> String {
        let mut out = String::new();
        if let Some(set) = &self.p_set {
            out.push_str(&format!("{}: {} ", set.base_index(), self.waypoint));
            out.push_str(&format!(
                "({}, {}, {})",
                self.point.x, self.point.y, self.point.z
            ));

            if self.waypoint != -1 {
                out.push_str(" - ");
                let wp = &set.waypoints()[self.waypoint as usize];
                let edge_count = wp.edge_count as usize;
                for i in 0..edge_count {
                    let start =
                        set.vertex_by_index(set.edges()[wp.edge_range().start + i].vertex_index);
                    out.push_str(&format!("({}, {})", start.x, start.y));
                    if i != edge_count - 1 {
                        out.push_str(", ");
                    }
                }
            }
        } else {
            out.push_str(&format!(
                "({}, {}, {}){}",
                self.point.x,
                self.point.y,
                self.point.z,
                if self.valid() { " " } else { " (invalid) " }
            ));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A decoded set: square (0,0)-(2,2) at height 0..1 and a right
    /// triangle (2,2)-(4,2)-(4,4) above it at 2..4, both clockwise in
    /// x-z so the reference projection test accepts interior points.
    fn sample_set() -> Rc<ChunkWaypointSetData> {
        let mut b = Vec::new();
        let put_f = |b: &mut Vec<u8>, v: f32| b.extend_from_slice(&v.to_le_bytes());
        let put_i = |b: &mut Vec<u8>, v: i32| b.extend_from_slice(&v.to_le_bytes());
        put_i(&mut b, 0);
        put_f(&mut b, 0.6);
        put_i(&mut b, 2);
        put_i(&mut b, 7);
        put_f(&mut b, 0.0);
        put_f(&mut b, 1.0);
        put_i(&mut b, 4);
        put_f(&mut b, 2.0);
        put_f(&mut b, 4.0);
        put_i(&mut b, 3);
        // square, clockwise in x-z like the engine's own navpolys
        for (x, y) in [(0.0f32, 0.0), (0.0, 2.0), (2.0, 2.0), (2.0, 0.0)] {
            put_f(&mut b, x);
            put_f(&mut b, y);
            put_i(&mut b, 65535);
        }
        // triangle above the square, clockwise
        for (x, y) in [(2.0f32, 2.0), (4.0, 4.0), (4.0, 2.0)] {
            put_f(&mut b, x);
            put_f(&mut b, y);
            put_i(&mut b, 65535);
        }
        let mut sets = crate::parse_world_navmesh(&b).expect("the sample set decodes");
        Rc::new(sets.remove(0))
    }

    #[test]
    fn the_default_result_is_invalid() {
        let loc = NavLoc::new();
        assert!(!loc.valid());
        assert_eq!(loc.waypoint(), 0);
        assert_eq!(loc.point(), Vector3::new(0.0, 0.0, 0.0));
        assert_eq!(loc.grid_size(), 0.0);
        assert!(!loc.is_within_wp());
        assert!(!loc.contains_projection(Vector3::new(1.0, 1.0, 1.0)));
        let mut p = Vector3::new(0.0, 0.0, 0.0);
        loc.make_max_height(&mut p);
        assert_eq!(p.y, 0.0, "an invalid location leaves the point alone");
        assert!(loc.desc().contains("(invalid)"));
    }

    #[test]
    fn the_guess_constructor_takes_the_same_waypoint_when_it_still_contains() {
        let set = sample_set();
        let lower = NavLoc {
            p_set: Some(Rc::clone(&set)),
            waypoint: 0,
            point: Vector3::new(1.0, 1.0, 1.0),
            grid_size: 0.0,
        };

        // A point that still projects into waypoint 0 (the height bump
        // comes with the constructor).
        let loc = NavLoc::from_guess(&lower, Rc::clone(&set), 1.5, Vector3::new(1.0, 0.5, 1.0));
        assert!(loc.valid());
        assert_eq!(loc.waypoint(), 0);
        assert!((loc.point().y - 0.51).abs() < 1e-6, "the +0.01 height bump");
        assert_eq!(loc.grid_size(), 1.5);
        assert!(loc.waypoints_equal(&lower));

        // A point over the upper triangle: the set search answers.
        let loc = NavLoc::from_guess(&lower, Rc::clone(&set), 1.5, Vector3::new(3.5, 3.0, 3.5));
        assert_eq!(loc.waypoint(), 1);
        assert!(!loc.waypoints_equal(&lower));

        // A point over no waypoint stays -1 (the space-search fallback is
        // registered out of this lane).
        let loc = NavLoc::from_guess(&lower, Rc::clone(&set), 1.5, Vector3::new(9.0, 0.5, 9.0));
        assert_eq!(loc.waypoint(), -1);
    }

    #[test]
    fn clip_pulls_the_stored_point_into_the_waypoint() {
        let set = sample_set();
        let mut loc = NavLoc {
            p_set: Some(Rc::clone(&set)),
            waypoint: 0,
            point: Vector3::new(1.0, 0.25, 1.0),
            grid_size: 0.0,
        };
        loc.clip();
        // `x`/`z` already sit inside wp0; `y` rises to its maxHeight.
        assert_eq!(loc.point(), Vector3::new(1.0, 1.0, 1.0));

        // An invalid location clips to nothing.
        let mut none = NavLoc::new();
        none.clip();
        assert_eq!(none.point(), Vector3::new(0.0, 0.0, 0.0));
    }

    #[test]
    fn the_guess_must_be_valid() {
        let set = sample_set();
        let invalid = NavLoc::new();
        assert!(
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                NavLoc::from_guess(&invalid, Rc::clone(&set), 1.5, Vector3::new(1.0, 1.0, 1.0))
            }))
            .is_err(),
            "reference MF_ASSERT_DEBUGs the guess is valid"
        );
    }

    #[test]
    fn is_within_wp_and_make_max_height_follow_the_waypoint() {
        let set = sample_set();
        let loc = NavLoc::from_guess(
            &NavLoc {
                p_set: Some(Rc::clone(&set)),
                waypoint: 0,
                point: Vector3::new(1.0, 1.0, 1.0),
                grid_size: 0.0,
            },
            Rc::clone(&set),
            0.0,
            Vector3::new(1.0, 0.5, 1.0),
        );
        assert!(loc.is_within_wp());

        let mut p = Vector3::new(1.0, 0.25, 1.0);
        loc.make_max_height(&mut p);
        assert_eq!(p.y, 1.0, "the lower waypoint's maxHeight_");

        let upper = NavLoc::from_guess(&loc, Rc::clone(&set), 0.0, Vector3::new(3.5, 2.5, 3.5));
        assert_eq!(upper.waypoint(), 1);
        upper.make_max_height(&mut p);
        assert_eq!(p.y, 4.0, "the upper waypoint's maxHeight_");
    }

    #[test]
    fn p_set_and_waypoints_equal_compare_the_set_object() {
        let set = sample_set();
        let loc = NavLoc {
            p_set: Some(Rc::clone(&set)),
            waypoint: 0,
            point: Vector3::new(1.0, 0.5, 1.0),
            grid_size: 0.0,
        };
        assert_eq!(loc.p_set().map(|s| s.girth()), Some(0.6));
        assert!(loc.p_set().unwrap().waypoints().len() == 2);

        // Same set object and the same waypoint index.
        let twin = NavLoc {
            p_set: Some(Rc::clone(&set)),
            waypoint: 0,
            point: Vector3::new(1.0, 0.5, 1.0),
            grid_size: 0.0,
        };
        assert!(loc.waypoints_equal(&twin));
        // A different waypoint on the same set is not equal.
        let other_wp = NavLoc {
            p_set: Some(Rc::clone(&set)),
            waypoint: 1,
            point: Vector3::new(3.0, 3.0, 3.0),
            grid_size: 0.0,
        };
        assert!(!loc.waypoints_equal(&other_wp));

        // Two invalid locations with the same waypoint compare equal, and
        // either against a located one does not.
        let none = NavLoc::new();
        assert!(none.waypoints_equal(&NavLoc::new()));
        assert!(!loc.waypoints_equal(&none));

        // A second decode of the same record is a different set object, so
        // the pointer comparison answers false despite the equal data.
        let second = sample_set();
        let twin = NavLoc {
            p_set: Some(second),
            waypoint: 0,
            point: Vector3::new(1.0, 0.5, 1.0),
            grid_size: 0.0,
        };
        assert!(!loc.waypoints_equal(&twin));
    }

    #[test]
    fn contains_projection_short_circuits_coincident_points() {
        let set = sample_set();
        let lower = NavLoc {
            p_set: Some(Rc::clone(&set)),
            waypoint: 0,
            point: Vector3::new(1.0, 1.0, 1.0),
            grid_size: 0.0,
        };

        // The 0.00001 window: the near-identical point answers without the
        // waypoint test.
        assert!(lower.contains_projection(Vector3::new(1.000004, 1.0, 1.0)));
        // A point off the waypoint's projection answers false.
        assert!(!lower.contains_projection(Vector3::new(1.0, 1.0, 5.0)));
    }

    #[test]
    fn desc_lists_the_set_the_waypoint_and_its_vertices() {
        let set = sample_set();
        let loc = NavLoc {
            p_set: Some(Rc::clone(&set)),
            waypoint: 0,
            point: Vector3::new(1.0, 0.5, 1.0),
            grid_size: 0.0,
        };
        let text = loc.desc();
        assert!(text.starts_with("1:"), "{text}");
        assert!(text.contains(": 0 "), "{text}");
        assert!(text.contains("(0, 0)"), "{text}");
        assert!(text.contains(", (2, 0)"), "{text}");

        // A located-but-unresolved waypoint prints no vertex list.
        let missing = NavLoc {
            p_set: Some(Rc::clone(&set)),
            waypoint: -1,
            point: Vector3::new(9.0, 0.5, 9.0),
            grid_size: 0.0,
        };
        assert_eq!(missing.desc(), "1: -1 (9, 0.5, 9)");

        let none = NavLoc::new();
        assert_eq!(none.desc(), "(0, 0, 0) (invalid) ");
    }
}
