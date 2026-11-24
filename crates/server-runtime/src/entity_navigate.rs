//! `server/cellapp/entity_navigate.{hpp,cpp}` and the mesh half of
//! `server/cellapp/navmesh_navigation_system.cpp` (BW-10-06-01): the
//! `Entity.navigate` script face — the `-1` max-search resolution, the
//! real/vehicle refusals, the exclusive movement controller creation —
//! over [`MeshNavigator`], the [`Navigator`] boundary implemented on
//! `waypoint`'s two-level search. `MeshNavigator` is the
//! `NavmeshEntityNavigationSystem` faces the controller touches:
//! `navigator_.createPathNavLoc(...)` (the space constructor + clip),
//! `getNavigatePosition(...)` (the same-waypoint shortcut and the
//! `findPath` hop with its height raise), `NavLoc::pSet()`, and
//! `getCachedWaypointPath`/`clearCachedWay{,Set}Path`.
//!
//! The reference's `createController`-failed `EnvironmentError` arm is
//! unreachable here as in the reference — `new NavigationController` only
//! fails on allocation — and is dropped (the `NAVIGATION_CANCELLED`
//! status-fidelity precedent); the deprecation note on `Entity.navigate`
//! lives in the reference docstring only.

use foundation::math::Vector3;
use std::rc::Rc;
use waypoint::{
    CacheState, ChunkWaypointSetData, NavLoc, Navigator as WaypointSearchNavigator, find_by_girth,
};

use crate::controllers::ControllerId;
use crate::navigation_controller::{NavigationController, Navigator};

// -----------------------------------------------------------------------------
// Section: MeshNavigator — the real mesh behind the Navigator boundary
// -----------------------------------------------------------------------------

/// The `NavmeshEntityNavigationSystem`'s `navigator_` over a loaded space:
/// the navmesh records plus the two-level search with its caches.
pub struct MeshNavigator {
    /// The space's loaded navmesh records
    /// (`pChunkSpace()->pNavmeshData()`), searched with the girth filter.
    sets: Vec<Rc<ChunkWaypointSetData>>,
    /// `navigator_` (`waypoint/navigator.hpp`).
    navigator: WaypointSearchNavigator,
    /// The space's `gridSize()` the navlocs carry (`navloc.cpp:66-70`).
    grid_size: f32,
}

impl MeshNavigator {
    /// Mount a space's records. `grid_size` is the space's grid size.
    pub fn new(sets: Vec<Rc<ChunkWaypointSetData>>, grid_size: f32) -> Self {
        Self {
            sets,
            navigator: WaypointSearchNavigator::default(),
            grid_size,
        }
    }

    /// `navigator_.createPathNavLoc( pChunkSpace, point, girth )` —
    /// `NavLoc( pChunkSpace, point, girth )` (`navloc.cpp:31-58`) plus the
    /// clip: the set is picked with the girth filter over the raw point
    /// (the space constructor's `+0.01` lift stays inside the chunk lookup
    /// it was written for), and a valid location is clipped into its
    /// waypoint.
    fn navloc_at(&self, point: Vector3, girth: f32) -> NavLoc {
        let res = find_by_girth(&self.sets, point, girth, false);
        let Some(set) = res.p_set() else {
            return NavLoc::new();
        };
        let mut loc = NavLoc::from_parts(Rc::clone(set), res.waypoint(), point, self.grid_size);
        loc.clip();
        loc
    }
}

impl Navigator for MeshNavigator {
    /// `NavLoc( pChunkSpace, point, girth )` + `clip()`: the mesh point,
    /// or `None` when the navloc is invalid (the reference's invalid
    /// navloc object folds into the same query).
    fn clip_to_mesh(&mut self, point: Vector3, girth: f32) -> Option<Vector3> {
        let loc = self.navloc_at(point, girth);
        if loc.valid() { Some(loc.point()) } else { None }
    }

    /// `NavmeshEntityNavigationSystem::getNavigatePosition`
    /// (`navmesh_navigation_system.cpp:550-627`): the same-waypoint
    /// shortcut answers the destination with the source's height raise;
    /// otherwise the `findPath` hop — the next waypoint's point, never
    /// lowered below the found `y`, with the height raise taken from the
    /// source when the hop is only semi-valid. `blockNonPermissive` is
    /// always true here and the search rides the read/write cache, the
    /// reference's default.
    ///
    /// `MF_ASSERT( srcLoc.valid() && dstLoc.valid() )` is kept as an
    /// assertion — the controller guarantees both through `clip_to_mesh`.
    fn navigate_position(
        &mut self,
        source: Vector3,
        destination: Vector3,
        max_distance: f32,
        girth: f32,
    ) -> Option<Vector3> {
        let src = self.navloc_at(source, girth);
        let dst = self.navloc_at(destination, girth);
        assert!(
            src.valid() && dst.valid(),
            "reference MF_ASSERTs the endpoints are valid"
        );

        // if they are the same waypoint it is easy
        if src.waypoints_equal(&dst) {
            let mut wp = dst.point();
            src.make_max_height(&mut wp);
            return Some(wp);
        }

        let mut way = NavLoc::new();
        let mut passed_activated_portal = false;
        if !self.navigator.find_path(
            &src,
            &dst,
            max_distance,
            true,
            &mut way,
            &mut passed_activated_portal,
            CacheState::ReadWriteCache,
        ) {
            return None;
        }

        // calculate the way world position, and ensure it is not too far
        // away; the height raise comes from the source when the hop is
        // only semi-valid.
        let mut wp = way.point();
        let dst_y = wp.y;
        let way_point = way.waypoint();
        let height_loc = if way_point < 0 { &src } else { &way };
        height_loc.make_max_height(&mut wp);
        wp.y = wp.y.max(dst_y);
        Some(wp)
    }

    /// `NavLoc::pSet()`: the identity of the waypoint set containing
    /// `point`, `None` when the navloc is invalid.
    fn waypoint_set(&mut self, point: Vector3, girth: f32) -> Option<u64> {
        let loc = self.navloc_at(point, girth);
        loc.p_set().map(|set| Rc::as_ptr(set) as u64)
    }

    /// `navigator().getCachedWaypointPath( path_ )`.
    fn cached_waypoint_path(&mut self) -> Vec<Vector3> {
        self.navigator.get_cached_waypoint_path()
    }

    /// `navigator().clearCachedWayPath()` + `clearCachedWaySetPath()`.
    fn clear_cached_paths(&mut self) {
        self.navigator.clear_cached_way_path();
        self.navigator.clear_cached_way_set_path();
    }
}

// -----------------------------------------------------------------------------
// Section: EntityNavigate — the Entity.navigate script face
// -----------------------------------------------------------------------------

/// The `NavigateFailure` faces — the reference's `PyErr_SetString` results.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NavigateFailure {
    /// `PyExc_TypeError`.
    NotReal,
    /// `PyExc_ValueError`.
    OnVehicle,
}

impl NavigateFailure {
    /// The reference's error text.
    pub fn message(&self) -> &'static str {
        match self {
            NavigateFailure::NotReal => "This method can only be called on a real entity.",
            NavigateFailure::OnVehicle => {
                "Cannot call Entity.navigate() for an entity on a vehicle, either \
                 alight from the vehicle first, or call Entity.navigate() for the \
                 vehicle."
            }
        }
    }
}

/// The `NavmeshEntityNavigationSystem` (`navmesh_navigation_system.hpp`):
/// the entity-bound system face `EntityNavigate` reaches through
/// `navigationSystem()`.
#[derive(Clone, Debug, Default)]
pub struct NavmeshEntityNavigationSystem;

impl NavmeshEntityNavigationSystem {
    /// `createController` (`navmesh_navigation_system.cpp:86-93`):
    /// `new NavigationController( ... )`. The reference returns `NULL`
    /// only when allocation fails — the port's constructor cannot fail —
    /// so this always answers (the `EnvironmentError` face is dropped with
    /// it, module doc).
    #[allow(clippy::too_many_arguments)]
    pub fn create_controller(
        &self,
        destination: Vector3,
        velocity: f32,
        face_movement: bool,
        max_search_distance: f32,
        girth: f32,
        close_enough: f32,
        controller_id: ControllerId,
        user_arg: i32,
        hertz: f32,
        max_aoi_radius: f32,
    ) -> NavigationController {
        NavigationController::new(
            controller_id,
            user_arg,
            destination,
            velocity,
            face_movement,
            max_search_distance,
            girth,
            close_enough,
            hertz,
            max_aoi_radius,
        )
    }
}

/// The `EntityNavigate` extra (`entity_navigate.hpp`): the entity facets
/// the script face reads, plus `navigationSystem()`.
#[derive(Clone, Debug, Default)]
pub struct EntityNavigate {
    /// `entity_.isRealToScript()`.
    pub is_real: bool,
    /// `entity_.pVehicle() != NULL`.
    pub on_vehicle: bool,
    /// `navigationSystem()` — created on demand in the reference, a plain
    /// member here.
    pub navigation_system: NavmeshEntityNavigationSystem,
}

impl EntityNavigate {
    /// `EntityNavigate::navigate` (`entity_navigate.cpp:597-637`): resolve
    /// the search limit, refuse non-real and mounted entities, then create
    /// the exclusive movement controller. The caller adds it and returns
    /// the controller id (`entity_.addController( pController, userArg )`).
    ///
    /// `isEqual( maxSearchDistance, -1.f )` is an exact test resolved to
    /// `CellAppConfig::maxAoIRadius()` before any refusal, as in the
    /// reference.
    #[allow(clippy::too_many_arguments)]
    pub fn navigate(
        &mut self,
        destination: Vector3,
        velocity: f32,
        face_movement: bool,
        max_search_distance: f32,
        girth: f32,
        close_enough: f32,
        user_arg: i32,
        controller_id: ControllerId,
        hertz: f32,
        max_aoi_radius: f32,
    ) -> Result<NavigationController, NavigateFailure> {
        let max_search_distance = if max_search_distance == -1.0 {
            max_aoi_radius
        } else {
            max_search_distance
        };

        if !self.is_real {
            return Err(NavigateFailure::NotReal);
        }

        if self.on_vehicle {
            return Err(NavigateFailure::OnVehicle);
        }

        Ok(self.navigation_system.create_controller(
            destination,
            velocity,
            face_movement,
            max_search_distance,
            girth,
            close_enough,
            controller_id,
            user_arg,
            hertz,
            max_aoi_radius,
        ))
    }
}

// -----------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::movement::{DEFAULT_GAME_UPDATE_HERTZ, Direction3D, EntityState, ScriptCallback};

    /// `CellAppConfig::maxAoIRadius()` stand-in.
    const MAX_AOI_RADIUS: f32 = 450.0;
    /// The gen scenes' space grid size.
    const GRID_SIZE: f32 = 1.0;
    /// The girth both gen records carry.
    const GIRTH: f32 = 0.8;

    /// The original generator's final stream for one scene: the `HX` hex
    /// dump under `"<label> bytes N"` in `gen_generate.log` is exactly the
    /// bytes navgen saves into `.cdata/worldNavmesh`.
    fn asset(label: &str) -> Vec<u8> {
        let log = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../docs/review/fixtures/waypoint/gen_generate.log"
        ))
        .expect("the generator truth log");
        let mut out = Vec::new();
        let mut declared: Option<usize> = None;
        let mut armed = false;
        for line in log.lines() {
            if armed && !line.starts_with("HX ") {
                break;
            }
            if !armed && line.starts_with(&format!("{label} bytes ")) {
                declared = Some(
                    line.split_whitespace()
                        .nth(2)
                        .expect("declared size")
                        .parse()
                        .unwrap(),
                );
                armed = true;
                continue;
            }
            if !armed {
                continue;
            }
            let hex = line.split_whitespace().nth(2).expect("hex column");
            for pair in (0..hex.len()).step_by(2) {
                out.push(u8::from_str_radix(&hex[pair..pair + 2], 16).expect("hex byte"));
            }
        }
        assert!(armed, "the {label} stream is in the log");
        assert_eq!(declared, Some(out.len()), "the {label} dump is complete");
        out
    }

    fn mesh_navigator(label: &str) -> MeshNavigator {
        let sets = waypoint::parse_world_navmesh(&asset(label))
            .expect("decode")
            .into_iter()
            .map(Rc::new)
            .collect();
        MeshNavigator::new(sets, GRID_SIZE)
    }

    fn real_entity() -> EntityNavigate {
        EntityNavigate {
            is_real: true,
            on_vehicle: false,
            navigation_system: NavmeshEntityNavigationSystem,
        }
    }

    /// The script-face refusals: a ghost and a mounted entity, with the
    /// reference's error texts.
    #[test]
    fn navigate_refuses_non_real_and_vehicle_entities() {
        let mut ghost = EntityNavigate::default();
        let err = ghost
            .navigate(
                Vector3::ZERO,
                1.0,
                true,
                -1.0,
                0.5,
                0.01,
                0,
                1,
                DEFAULT_GAME_UPDATE_HERTZ,
                MAX_AOI_RADIUS,
            )
            .expect_err("a ghost cannot navigate");
        assert_eq!(err, NavigateFailure::NotReal);
        assert_eq!(
            err.message(),
            "This method can only be called on a real entity."
        );

        let mut mounted = EntityNavigate {
            is_real: true,
            on_vehicle: true,
            navigation_system: NavmeshEntityNavigationSystem,
        };
        let err = mounted
            .navigate(
                Vector3::ZERO,
                1.0,
                true,
                -1.0,
                0.5,
                0.01,
                0,
                1,
                DEFAULT_GAME_UPDATE_HERTZ,
                MAX_AOI_RADIUS,
            )
            .expect_err("a mounted entity cannot navigate");
        assert_eq!(err, NavigateFailure::OnVehicle);
        assert_eq!(
            err.message(),
            "Cannot call Entity.navigate() for an entity on a vehicle, either \
             alight from the vehicle first, or call Entity.navigate() for the \
             vehicle."
        );
    }

    /// The `-1` search limit resolves to the AOI radius before the
    /// controller is created; an explicit limit is kept; velocity and
    /// `userArg` land in the controller.
    #[test]
    fn navigate_resolves_the_search_limit_and_creates_the_controller() {
        let mut extra = real_entity();
        let controller = extra
            .navigate(
                Vector3::new(7.0, 3.0, 7.0),
                2.0,
                true,
                -1.0,
                GIRTH,
                0.4,
                42,
                4,
                10.0,
                MAX_AOI_RADIUS,
            )
            .expect("a real grounded entity navigates");
        assert!((controller.max_distance - MAX_AOI_RADIUS).abs() < 1e-5);
        assert!((controller.metres_per_tick - 0.2).abs() < 1e-5);
        assert_eq!(controller.destination, Vector3::new(7.0, 3.0, 7.0));

        let controller = extra
            .navigate(
                Vector3::new(7.0, 3.0, 7.0),
                2.0,
                true,
                30.0,
                GIRTH,
                0.4,
                42,
                4,
                10.0,
                MAX_AOI_RADIUS,
            )
            .expect("the explicit limit is kept");
        assert!((controller.max_distance - 30.0).abs() < 1e-5);
    }

    /// BW-10-06-01 acceptance — 固定路线到达时触发对应回调: over the
    /// original generator's slope asset, `navigate` walks the fixed route
    /// (source wp1, the mesh crossing at (3, 3.5, 3), the destination in
    /// wp0) and arrival fires `onNavigate` with the controller id and
    /// `userArg`.
    #[test]
    fn a_fixed_route_arrival_fires_the_on_navigate_callback() {
        let mut navigator = mesh_navigator("slope");
        let mut extra = real_entity();
        let mut controller = extra
            .navigate(
                Vector3::new(7.0, 3.0, 7.0),
                2.0,
                true,
                -1.0,
                GIRTH,
                0.4,
                42,
                4,
                10.0,
                MAX_AOI_RADIUS,
            )
            .expect("the controller is created");

        let mut state = EntityState::new(7, Vector3::new(1.0, 0.5, 1.0), Direction3D::default(), 0);
        controller.start(&mut state, &mut navigator, true);
        assert!(controller.registered);
        // The clip face: the destination is pulled into wp0 — `x`/`z`
        // stay, `y` rises to the waypoint's maxHeight (3.5).
        assert_eq!(controller.destination, Vector3::new(7.0, 3.5, 7.0));
        // The mesh crossing is the first hop of the fixed route.
        assert!(
            (controller.next_position.x - 3.0).abs() < 1e-4
                && (controller.next_position.z - 3.0).abs() < 1e-4,
            "the first hop is the crossing, got {:?}",
            controller.next_position
        );

        let mut now = 0u32;
        while !controller.update(&mut state, &mut navigator, now + 1, 10.0) {
            now += 1;
            assert!(now < 200, "must arrive");
        }

        // The entity finished inside the closeEnough window of the
        // destination (the reference completes mid-hop the moment the
        // window catches) and walks the clipped height.
        assert!(
            (state.position.x - 7.0).abs() <= 0.4
                && (state.position.z - 7.0).abs() <= 0.4
                && (state.position.y - 3.5).abs() < 1e-4,
            "arrived at {:?}",
            state.position
        );
        assert_eq!(
            controller.events,
            vec![ScriptCallback {
                entity_id: 7,
                method: "onNavigate",
                controller_id: 4,
                user_arg: 42,
            }]
        );
        assert!(controller.is_cancelled());
    }

    /// The shell's floor and bridge layers carry no inter-set adjacency,
    /// so a cross-layer route fails: `onNavigateFailed` fires and the
    /// entity never leaves its start point.
    #[test]
    fn a_route_across_disconnected_layers_fails_with_the_failure_callback() {
        let mut navigator = mesh_navigator("shell");
        let mut extra = real_entity();
        let mut controller = extra
            .navigate(
                Vector3::new(3.0, 1.5, 3.5),
                2.0,
                true,
                -1.0,
                GIRTH,
                0.4,
                7,
                5,
                10.0,
                MAX_AOI_RADIUS,
            )
            .expect("the controller is created");

        let start = Vector3::new(2.5, 0.5, 4.5);
        let mut state = EntityState::new(7, start, Direction3D::default(), 0);
        controller.start(&mut state, &mut navigator, true);

        let mut now = 0u32;
        while !controller.update(&mut state, &mut navigator, now + 1, 10.0) {
            now += 1;
            assert!(now < 10, "fails on the first re-query");
        }
        assert_eq!(state.position, start, "the entity never moved");
        assert_eq!(
            controller.events,
            vec![ScriptCallback {
                entity_id: 7,
                method: "onNavigateFailed",
                controller_id: 5,
                user_arg: 7,
            }]
        );
    }

    /// The mesh faces the layer probes answer through: a clip (the pull-in
    /// keeps `x`/`z` and raises `y` to the located waypoint's maxHeight),
    /// the set identity, the same-waypoint shortcut (the destination
    /// answered with the source's height raise), and the invalid faces.
    #[test]
    fn the_mesh_faces_answer_the_layer_probes() {
        let mut navigator = mesh_navigator("slope");

        // Clip: on the mesh (both layers). A wrong girth has no record to
        // answer with; a far point does not hit this face — the closest
        // fallback still locates it on the matching girth.
        assert_eq!(
            navigator.clip_to_mesh(Vector3::new(1.0, 0.5, 1.0), GIRTH),
            Some(Vector3::new(1.0, 1.5, 1.0))
        );
        assert_eq!(
            navigator.clip_to_mesh(Vector3::new(7.0, 3.0, 7.0), GIRTH),
            Some(Vector3::new(7.0, 3.5, 7.0))
        );
        assert_eq!(
            navigator.clip_to_mesh(Vector3::new(50.0, 50.0, 50.0), 0.5),
            None
        );

        // The set identity: both slope points land in the one set.
        let src_set = navigator.waypoint_set(Vector3::new(1.0, 0.5, 1.0), GIRTH);
        assert!(src_set.is_some());
        assert_eq!(
            navigator.waypoint_set(Vector3::new(7.0, 3.0, 7.0), GIRTH),
            src_set
        );
        assert_eq!(
            navigator.waypoint_set(Vector3::new(50.0, 50.0, 50.0), 0.5),
            None
        );

        // Same-waypoint shortcut: both points sit in wp1, so the answer is
        // the destination with the waypoint's max height (1.5).
        assert_eq!(
            navigator.navigate_position(
                Vector3::new(1.0, 0.5, 1.0),
                Vector3::new(1.5, 0.5, 1.5),
                -1.0,
                GIRTH
            ),
            Some(Vector3::new(1.5, 1.5, 1.5))
        );

        // The cache faces answer through the boundary: nothing was cached
        // by the shortcut arm, so the path view is empty.
        navigator.clear_cached_paths();
        assert!(navigator.cached_waypoint_path().is_empty());
    }

    /// The invalid-endpoint assertion (`MF_ASSERT` face), probed with a
    /// girth no record matches — every navloc then resolves invalid.
    #[test]
    #[should_panic(expected = "reference MF_ASSERTs the endpoints are valid")]
    fn navigate_position_asserts_invalid_endpoints() {
        let mut navigator = mesh_navigator("slope");
        let _ = navigator.navigate_position(
            Vector3::new(1.0, 0.5, 1.0),
            Vector3::new(50.0, 50.0, 50.0),
            -1.0,
            0.5,
        );
    }
}
