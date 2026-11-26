//! BW-10-07-04: the client query faces over the camera space
//! (`client/script_bigworld.cpp`): `BigWorld.findDropPoint` and
//! `BigWorld.navigatePathPoints`.
//!
//! `findDropPoint` walks the collision scene under a point and answers
//! the drop position plus the triangle it hit; `navigatePathPoints`
//! walks the waypoint graph between two points through
//! `Navigator::findFullPath` with `blockNonPermissive` hard-coded true
//! and the script default girth 0.5.

use chunk::ChunkSpace;
use foundation::math::vector::Vector3;
use physics::ClosestTriangle;
use waypoint::{NavLoc, Navigator};

/// The `findDropPoint` failure face (`script_bigworld.cpp:137-142`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FindDropPointFailure {
    /// The space ID the reference formats into the text.
    pub space_id: u16,
}

impl FindDropPointFailure {
    /// `PyExc_ValueError` — no space with this ID.
    pub fn message(&self) -> String {
        format!("BigWorld.findDropPoint(): No space ID {}", self.space_id)
    }
}

/// The `navigatePathPoints` failure faces
/// (`script_bigworld.cpp:1886-1890,1922-1927`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NavigatePathPointsFailure {
    /// `PyExc_ValueError` — `ChunkManager::cameraSpace()` answered none.
    NoCameraSpace,
    /// `PyExc_ValueError` — `findFullPath` answered false.
    PathNotFound,
}

impl NavigatePathPointsFailure {
    /// The reference's error text.
    pub fn message(&self) -> &'static str {
        match self {
            NavigatePathPointsFailure::NoCameraSpace => {
                "BigWorld.navigatePathPoints: Cannot get camera space"
            }
            NavigatePathPointsFailure::PathNotFound => {
                "BigWorld.navigatePathPoints: Cannot find path points. \
                 Check that client side navigation is enabled in \
                 space.settings."
            }
        }
    }
}

/// The `almostEqual(Vector3, Vector3)` of the prepend rule
/// (`lib/math/vector3.hpp:131-136`): component-wise, the scalar default
/// epsilon 0.0004, strict `<`.
fn almost_equal_point(a: Vector3, b: Vector3) -> bool {
    let epsilon = 0.0004f32;
    (a.x - b.x).abs() < epsilon && (a.y - b.y).abs() < epsilon && (a.z - b.z).abs() < epsilon
}

/// `BigWorld.findDropPoint` (`script_bigworld.cpp:129-159`): the drop
/// point directly beneath `in_pt` and the triangle it hit, or `None`
/// when the ray missed.
///
/// `space` plays the `SpaceManager::instance().space( spaceID )` lookup
/// result — the caller drives the lookup, so the miss carries the ID the
/// reference formats into the error text.
///
/// The recorded triangle rides the caller's `ClosestTriangle` probe
/// (`fdpt`), mirroring the reference's stack probe.
pub fn find_drop_point(
    space: Option<&ChunkSpace>,
    space_id: u16,
    in_pt: Vector3,
    fdpt: &mut ClosestTriangle,
) -> Result<Option<(Vector3, [Vector3; 3])>, FindDropPointFailure> {
    let Some(space) = space else {
        return Err(FindDropPointFailure { space_id });
    };

    let nd_pt = Vector3::new(in_pt.x, in_pt.y - 100.0, in_pt.z);
    let dist = space.collide(in_pt, nd_pt, fdpt);
    if dist < 0.0 {
        return Ok(None);
    }
    let out_pt = Vector3::new(in_pt.x, in_pt.y - dist, in_pt.z);
    let tri = fdpt.triangle();
    Ok(Some((out_pt, [tri.v0(), tri.v1(), tri.v2()])))
}

/// `BigWorld.navigatePathPoints` (`script_bigworld.cpp:1880-1929`): the
/// waypoint path between `src` and `dst` through the camera space, with
/// the raw `src` prepended when `findFullPath`'s first point differs
/// from it (the reference's `almostEqual` guard).
///
/// The camera-space lookup result plays the `ChunkManager::cameraSpace()`
/// gate; the resolved `NavLoc` pair plays the construction
/// `Navigator::findFullPath` performs internally through the space
/// constructor (`navloc.cpp:41-64`) — the port's registered boundary (the
/// replay harness supplies them already resolved; see
/// `crates/waypoint/src/navigator.rs`). The raw `_dst` therefore stays
/// out of the port's body: the reference feeds it to the destination's
/// own `NavLoc` constructor, which this face takes as `dst_navloc`.
/// `blockNonPermissive` is hard-coded true in the reference call.
#[allow(clippy::too_many_arguments)]
pub fn navigate_path_points(
    camera_space: Option<&ChunkSpace>,
    navigator: &Navigator,
    src_navloc: &NavLoc,
    dst_navloc: &NavLoc,
    src: Vector3,
    _dst: Vector3,
    max_search_distance: f32,
) -> Result<Vec<Vector3>, NavigatePathPointsFailure> {
    if camera_space.is_none() {
        return Err(NavigatePathPointsFailure::NoCameraSpace);
    }

    let mut path_points = Vec::new();
    let found = navigator.find_full_path(
        src_navloc,
        dst_navloc,
        max_search_distance,
        /* block_non_permissive: */ true,
        &mut path_points,
    );

    if !found {
        return Err(NavigatePathPointsFailure::PathNotFound);
    }

    debug_assert!(
        !path_points.is_empty(),
        "the reference MF_ASSERTs a found path is non-empty"
    );

    if !almost_equal_point(path_points[0], src) {
        path_points.insert(0, src);
    }

    Ok(path_points)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chunk::GeometryMapping;
    use std::rc::Rc;
    use waypoint::ChunkWaypointSetData;

    fn v(x: f32, y: f32, z: f32) -> Vector3 {
        Vector3::new(x, y, z)
    }

    /// One girth-0.6 set shaped like the fixture map's (the decode path
    /// of `docs/review/fixtures/waypoint/world_navmesh_genmap_a.bin`):
    /// a single waypoint on a four-vertex square.
    fn one_set() -> Rc<ChunkWaypointSetData> {
        let mut b = Vec::new();
        let put_f = |out: &mut Vec<u8>, f: f32| out.extend(&f.to_le_bytes());
        let put_i = |out: &mut Vec<u8>, i: i32| out.extend(&i.to_le_bytes());
        put_i(&mut b, 0); // version
        put_f(&mut b, 0.6); // girth
        put_i(&mut b, 1); // waypoints
        put_i(&mut b, 4); // edges
        put_f(&mut b, 0.2); // waypoint x
        put_f(&mut b, 0.6); // waypoint y
        put_i(&mut b, 4); // the waypoint's edge count
        for (x, y) in [(0.0f32, 0.0), (0.0, 1.0), (1.0, 1.0), (1.0, 0.0)] {
            put_f(&mut b, x);
            put_f(&mut b, y);
            put_i(&mut b, 65535);
        }
        Rc::new(waypoint::parse_world_navmesh(&b).expect("decode").remove(0))
    }

    fn at(set: &Rc<ChunkWaypointSetData>, x: f32) -> NavLoc {
        NavLoc::from_parts(Rc::clone(set), 0, v(x, 0.4, 0.5), 0.0)
    }

    /// A grid-100 mapping with no chunks: the space walks but answers
    /// nothing (the truth log's GNONE probe shape).
    fn empty_mapping() -> GeometryMapping {
        use chunk::GridBounds;
        GeometryMapping::new(
            "space/",
            100.0,
            GridBounds {
                min_l_grid_x: 0,
                min_l_grid_y: 0,
                max_l_grid_x: 1,
                max_l_grid_y: 1,
                min_grid_x: 0,
                min_grid_y: 0,
                max_grid_x: 1,
                max_grid_y: 1,
            },
            false,
        )
    }

    #[test]
    fn find_drop_point_misses_the_lookup_with_the_reference_text() {
        let err = find_drop_point(None, 7, v(1.0, 5.0, 1.0), &mut ClosestTriangle::new())
            .expect_err("the lookup missed");
        assert_eq!(err.space_id, 7);
        assert_eq!(err.message(), "BigWorld.findDropPoint(): No space ID 7");
    }

    #[test]
    fn find_drop_point_answers_none_when_the_ray_misses() {
        // a mapped but empty space: the grid walk runs yet no column
        // holds an obstacle, so every ray misses (the reference crashes
        // on a mapping-less space in gridSize, hence the mapping here)
        let space = ChunkSpace::new(7, vec![empty_mapping()]);
        let mut fdpt = ClosestTriangle::new();
        let hit = find_drop_point(Some(&space), 7, v(1.0, 5.0, 1.0), &mut fdpt)
            .expect("the space was found");
        assert_eq!(hit, None);
    }

    #[test]
    fn navigate_path_points_refuses_the_missing_camera_space() {
        let set = one_set();
        let navigator = Navigator::default();
        let err = navigate_path_points(
            None,
            &navigator,
            &at(&set, 0.2),
            &at(&set, 0.8),
            v(0.2, 0.4, 0.5),
            v(0.8, 0.4, 0.5),
            500.0,
        )
        .expect_err("no camera space");
        assert_eq!(err, NavigatePathPointsFailure::NoCameraSpace);
        assert_eq!(
            err.message(),
            "BigWorld.navigatePathPoints: Cannot get camera space"
        );
    }

    #[test]
    fn navigate_path_points_reports_the_no_path_error() {
        let space = ChunkSpace::new(7, Vec::<GeometryMapping>::new());
        let navigator = Navigator::default();
        // unresolved navlocs (waypoint -1, no set): the walk never
        // starts, so findFullPath answers false
        let err = navigate_path_points(
            Some(&space),
            &navigator,
            &NavLoc::new(),
            &NavLoc::new(),
            v(0.2, 0.4, 0.5),
            v(0.8, 0.4, 0.5),
            500.0,
        )
        .expect_err("no adjacency between the endpoints");
        assert_eq!(err, NavigatePathPointsFailure::PathNotFound);
        assert_eq!(
            err.message(),
            "BigWorld.navigatePathPoints: Cannot find path points. \
             Check that client side navigation is enabled in \
             space.settings."
        );
    }

    #[test]
    fn navigate_path_points_prepends_the_raw_source_when_the_first_point_differs() {
        let (a, b) = wired_pair();
        let space = ChunkSpace::new(7, Vec::<GeometryMapping>::new());
        let navigator = Navigator::default();
        // the raw script source sits off the waypoint centre, so
        // findFullPath's first clipped point differs from it and the
        // guard prepends
        let raw_src = v(0.05, 0.4, 0.5);
        let path = navigate_path_points(
            Some(&space),
            &navigator,
            &at(&a, 0.2),
            &at(&b, 0.8),
            raw_src,
            v(0.8, 0.4, 0.5),
            1_000_000.0,
        )
        .expect("the wired pair walks");
        assert!(path.len() >= 3, "{path:?}");
        assert_eq!(path[0], raw_src, "the raw source is prepended");
        assert!(
            !almost_equal_point(path[1], raw_src),
            "the walk's own first point differs from the raw source {path:?}"
        );
        // the walk's own last point is the destination's clip into its
        // waypoint, so only its horizontal position is pinned
        let last = *path.last().expect("non-empty");
        assert!((last.x - 0.8).abs() < 0.0004, "{path:?}");
    }

    /// Two sets with one permissive portal connection A → B, mounted on
    /// chunk identities the cross-set arm accepts.
    fn wired_pair() -> (Rc<ChunkWaypointSetData>, Rc<ChunkWaypointSetData>) {
        use waypoint::Portal;
        let a = one_set();
        let b = one_set();
        a.set_chunk_identity(0);
        b.set_chunk_identity(1);
        let portal = Rc::new(Portal {
            permissive: true,
            has_chunk_item: false,
        });
        a.connect(b.clone(), portal.clone(), 2, 0, v(0.5, 0.5, 0.5));
        b.connect(a.clone(), portal, 0, 0, v(0.5, 0.5, 0.5));
        (a, b)
    }
}
