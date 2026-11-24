//! `server/cellapp/entity_navigate.{hpp,cpp}` and the mesh half of
//! `server/cellapp/navmesh_navigation_system.cpp` (BW-10-06-01): the
//! `Entity.navigate`/`navigateStep`/`canNavigateTo` script faces — the
//! `-1` max-search resolution, the real/vehicle refusals, the exclusive
//! movement controller creation, the read-only reachability probe —
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
use crate::movement::{EntityState, NavigateStepController};
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
    /// `NavLoc( pChunkSpace, point, girth )` — the space constructor face
    /// without the clip: `navigateStep` resolves its destination with it
    /// and clips only after the validity check
    /// (`navmesh_navigation_system.cpp:353-362`).
    fn navloc_unclipped(&self, point: Vector3, girth: f32) -> NavLoc {
        let res = find_by_girth(&self.sets, point, girth, false);
        let Some(set) = res.p_set() else {
            return NavLoc::new();
        };
        NavLoc::from_parts(Rc::clone(set), res.waypoint(), point, self.grid_size)
    }

    /// `navigator_.createPathNavLoc( pChunkSpace, point, girth )` —
    /// `NavLoc( pChunkSpace, point, girth )` (`navloc.cpp:31-58`) plus the
    /// clip: the set is picked with the girth filter over the raw point
    /// (the space constructor's `+0.01` lift stays inside the chunk lookup
    /// it was written for), and a valid location is clipped into its
    /// waypoint.
    fn navloc_at(&self, point: Vector3, girth: f32) -> NavLoc {
        let mut loc = self.navloc_unclipped(point, girth);
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

/// The `navigateStep` failure faces — the reference's `PyErr` results
/// (`entity_navigate.cpp:783-799`, `navmesh_navigation_system.cpp:332-376`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NavigateStepFailure {
    /// `PyExc_TypeError`.
    NotReal,
    /// `PyExc_ValueError`, the vehicle text.
    OnVehicle,
    /// `!entity_.pChunkSpace()`.
    NoSpace,
    /// The source navloc is invalid.
    SourceUnresolved,
    /// The destination navloc is invalid.
    DestinationUnresolved,
    /// No route within the search limit.
    NoPath,
}

impl NavigateStepFailure {
    /// The reference's error text.
    pub fn message(&self) -> &'static str {
        match self {
            NavigateStepFailure::NotReal => "This method can only be called on a real entity.",
            NavigateStepFailure::OnVehicle => {
                "Cannot call Entity.navigateStep() for an entity on a vehicle, either \
                 alight from the vehicle first, or call Entity.navigateStep() for the \
                 vehicle."
            }
            NavigateStepFailure::NoSpace => {
                "NavmeshEntityNavigationSystem::navigateStep(): \
                 Physical space of entity does not support navigation queries"
            }
            NavigateStepFailure::SourceUnresolved => "Could not resolve source position.",
            NavigateStepFailure::DestinationUnresolved => "Could not resolve destination position.",
            NavigateStepFailure::NoPath => "No path found.",
        }
    }
}

/// The `canNavigateTo` failure faces — the reference's `PyErr` results
/// (`entity_navigate.cpp:966-970`, `navmesh_navigation_system.cpp:434-441`).
/// Unresolved endpoints answer `None`, not an error, on this face.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CanNavigateFailure {
    /// `PyExc_TypeError`.
    NotReal,
    /// `!entity_.pChunkSpace()`.
    NoSpace,
}

impl CanNavigateFailure {
    /// The reference's error text.
    pub fn message(&self) -> &'static str {
        match self {
            CanNavigateFailure::NotReal => "This method can only be called on a real entity.",
            CanNavigateFailure::NoSpace => {
                "NavmeshEntityNavigationSystem::canNavigateTo(): \
                 Physical space of entity does not support navigation queries"
            }
        }
    }
}

/// `almostZero( f )` (`cstdmf/stdmf.hpp:616`): the reference's default
/// epsilon is `0.0004`, the same helper the physics crate ports.
fn almost_zero(f: f32) -> bool {
    -0.0004 < f && f < 0.0004
}

/// The entity `navigateFollow` follows — the `Entity` facets the reference
/// reads off it (`entity_navigate.cpp:882-895`).
#[derive(Clone, Copy, Debug)]
pub struct FollowTarget {
    /// `PyObject_IsInstance( pEntity, &Entity::s_type_ )`.
    pub is_entity: bool,
    /// `pOtherEntity->isDestroyed()`.
    pub destroyed: bool,
    /// `entity().space().id() == pOtherEntity->space().id()`.
    pub same_space: bool,
    /// `pOtherEntity->position()`.
    pub position: Vector3,
    /// `pOtherEntity->direction().yaw`.
    pub yaw: f32,
}

/// The `navigateFollow` failure faces
/// (`entity_navigate.cpp:884-915`), the follow-specific checks first and
/// then the delegated `navigateStep` refusals.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NavigateFollowFailure {
    /// `PyExc_TypeError`.
    NotEntity,
    /// `PyExc_ValueError`.
    Destroyed,
    /// `PyExc_ValueError`.
    DifferentSpace,
    /// `PyExc_ValueError`.
    ZeroSearchDistance,
    /// A `navigateStep` refusal the delegation passes through.
    NavigateStep(NavigateStepFailure),
}

impl NavigateFollowFailure {
    /// The reference's error text.
    pub fn message(&self) -> &'static str {
        match self {
            NavigateFollowFailure::NotEntity => "parameter 1 must be an Entity instance",
            NavigateFollowFailure::Destroyed => "cannot follow a destroyed entity",
            NavigateFollowFailure::DifferentSpace => "followed entity is not in the same space",
            NavigateFollowFailure::ZeroSearchDistance => "maxSearchDistance cannot be 0.0",
            NavigateFollowFailure::NavigateStep(inner) => inner.message(),
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
/// the script face reads, plus `navigationSystem()` and the attached
/// navigate-step controller.
#[derive(Clone, Debug, Default)]
pub struct EntityNavigate {
    /// `entity_.isRealToScript()`.
    pub is_real: bool,
    /// `entity_.pVehicle() != NULL`.
    pub on_vehicle: bool,
    /// `navigationSystem()` — created on demand in the reference, a plain
    /// member here.
    pub navigation_system: NavmeshEntityNavigationSystem,
    /// `pNavigateStepController_` — the step controller
    /// `registerNavigateStepController` attached (`entity_navigate.cpp:86-91`);
    /// owning it models the registration.
    pub navigate_step: Option<NavigateStepController>,
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

    /// `deregisterNavigateStepController` (`entity_navigate.cpp:93-100`):
    /// the attached step controller is cleared when the id matches.
    pub fn deregister_navigate_step_controller(&mut self, id: ControllerId) {
        if self.navigate_step.as_ref().map(|c| c.id) == Some(id) {
            self.navigate_step = None;
        }
    }

    /// `EntityNavigate::navigateStep` (`entity_navigate.cpp:775-806`) and
    /// the `NavmeshEntityNavigationSystem::navigateStep` body
    /// (`navmesh_navigation_system.cpp:328-434`): resolve the `-1` search
    /// limit, refuse non-real and mounted entities, resolve both navlocs
    /// (the source through `createPathNavLoc` — space constructor + clip;
    /// the destination through the bare space constructor, clipped only
    /// after the validity check), ask `getNavigatePosition` for the single
    /// step, honour `maxMoveDistance` by trimming the step along its
    /// `x`/`z` displacement, drop the entity's `y` onto the step now (the
    /// reference's `setAndDropGlobalPositionAndDirection`), and create the
    /// step controller or — when one is already attached — retarget it.
    ///
    /// The `srcLoc.makeMaxHeight` of the trim arm goes through the source
    /// navloc's own waypoint projection window
    /// (`NavLoc::containsProjection`); the answer is the controller id
    /// either way, as the reference's `Script::getData( id() )`.
    #[allow(clippy::too_many_arguments)]
    pub fn navigate_step(
        &mut self,
        state: &mut EntityState,
        navigator: &mut MeshNavigator,
        destination: Vector3,
        velocity: f32,
        max_move_distance: f32,
        max_search_distance: f32,
        face_movement: bool,
        girth: f32,
        user_arg: i32,
        hertz: f32,
        max_aoi_radius: f32,
        next_controller_id: ControllerId,
        chunk_space: bool,
        now: u32,
    ) -> Result<ControllerId, NavigateStepFailure> {
        // isEqual( maxSearchDistance, -1.f ) — resolved before the
        // refusals, as in navigate().
        let max_search_distance = if max_search_distance == -1.0 {
            max_aoi_radius
        } else {
            max_search_distance
        };

        if !self.is_real {
            return Err(NavigateStepFailure::NotReal);
        }

        if self.on_vehicle {
            return Err(NavigateStepFailure::OnVehicle);
        }

        // ToDo in the reference: change when navigation system supports
        // non-grid spaces.
        if !chunk_space {
            return Err(NavigateStepFailure::NoSpace);
        }

        // figure out the source navloc
        let src = navigator.navloc_at(state.position, girth);

        // complain if invalid
        if !src.valid() {
            return Err(NavigateStepFailure::SourceUnresolved);
        }

        // figure out the dest navloc, then clip it
        let mut dst = navigator.navloc_unclipped(destination, girth);
        if !dst.valid() {
            return Err(NavigateStepFailure::DestinationUnresolved);
        }
        dst.clip();

        // getNavigatePosition: the same-waypoint shortcut or the findPath
        // hop (the clipped endpoints re-resolve onto their own waypoints).
        let Some(mut next_position) =
            navigator.navigate_position(src.point(), dst.point(), max_search_distance, girth)
        else {
            return Err(NavigateStepFailure::NoPath);
        };

        let mut disp = next_position - state.position;
        disp.y = 0.0;
        let way_distance = disp.length();
        let mut dst_y = next_position.y;

        let mut wsv = next_position;
        src.make_max_height(&mut wsv);
        let src_y = wsv.y;

        if way_distance > max_move_distance {
            // shorten it
            next_position = state.position + disp * (max_move_distance / way_distance);

            if src.contains_projection(next_position) {
                dst_y = src_y;
            }
        }

        // set our y position now
        next_position.y = dst_y.max(src_y);
        let mut position = state.position;
        position.y = next_position.y;

        // since not on a vehicle, so it's ok to pass global
        // position/direction (the drop face is the ghost update, as in the
        // navigation controller).
        state.set_position_and_direction(position, state.direction, now, hertz);

        if let Some(controller) = &mut self.navigate_step {
            controller.set_destination(next_position);
            Ok(controller.id)
        } else {
            let controller = NavigateStepController::new(
                next_controller_id,
                user_arg,
                next_position,
                velocity,
                face_movement,
                hertz,
            );
            let id = controller.id;
            self.navigate_step = Some(controller);
            Ok(id)
        }
    }

    /// `EntityNavigate::canNavigateTo` (`entity_navigate.cpp:957-976`) and
    /// the `NavmeshEntityNavigationSystem::canNavigateTo` body
    /// (`navmesh_navigation_system.cpp:431-518`): resolve the `-1` search
    /// limit, refuse non-real entities — the reference has **no** vehicle
    /// refusal on this face — then resolve both navlocs like `navigateStep`
    /// (source through `createPathNavLoc`, destination through the bare
    /// space constructor clipped after its check). Unresolved endpoints
    /// answer `None` here, not an error. The same-waypoint shortcut
    /// answers the clipped destination with the **source** height raise;
    /// otherwise a `findPath` probe on the read-only cache face decides —
    /// found answers the clipped destination with the **destination**
    /// height raise, unfound answers `None`. The reference's
    /// `navigator().clearCache()` before the probe stays commented out.
    #[allow(clippy::too_many_arguments)]
    pub fn can_navigate_to(
        &self,
        state: &EntityState,
        navigator: &MeshNavigator,
        dst_position: Vector3,
        max_search_distance: f32,
        girth: f32,
        max_aoi_radius: f32,
        chunk_space: bool,
    ) -> Result<Option<Vector3>, CanNavigateFailure> {
        // isEqual( maxSearchDistance, -1.f ) — resolved before the
        // refusal, as in navigate()/navigateStep().
        let max_search_distance = if max_search_distance == -1.0 {
            max_aoi_radius
        } else {
            max_search_distance
        };

        if !self.is_real {
            return Err(CanNavigateFailure::NotReal);
        }

        // ToDo in the reference: change when navigation system supports
        // non-grid spaces.
        if !chunk_space {
            return Err(CanNavigateFailure::NoSpace);
        }

        // figure out the source navloc
        let src = navigator.navloc_at(state.position, girth);

        // complain if invalid — silently, on this face
        if !src.valid() {
            return Ok(None);
        }

        // figure out the dest navloc, then clip it — the reference's
        // destination-invalid arm is unreachable like its navigateStep
        // twin: both endpoints resolve through the same girth filter, so
        // a valid source implies a valid destination.
        let mut dst = navigator.navloc_unclipped(dst_position, girth);
        if !dst.valid() {
            return Ok(None);
        }
        dst.clip();

        // if they are the same waypoint it is easy
        if src.waypoints_equal(&dst) {
            let mut wp = dst.point();
            src.make_max_height(&mut wp);
            return Ok(Some(wp));
        }

        // Need to clear navigation cache. It is possible that when a door
        // was open and a search from srcLoc -> dstLoc was stored, which is
        // still there. The door may now be closed. — the reference keeps
        // the clearCache call commented out.
        let mut way = NavLoc::new();
        let mut passed_activated_portal = false;
        let found = navigator.navigator.find_path(
            &src,
            &dst,
            max_search_distance,
            /* block_non_permissive: */ true,
            &mut way,
            &mut passed_activated_portal,
            CacheState::ReadCache,
        );

        if !found {
            return Ok(None);
        }

        let mut wp = dst.point();
        dst.make_max_height(&mut wp);
        Ok(Some(wp))
    }

    /// `EntityNavigate::navigateFollow` (`entity_navigate.cpp:872-922`,
    /// deprecated in the reference's docstring): resolve the `-1` search
    /// limit, check the target is an `Entity` in the same space and not
    /// destroyed, reject an almost-zero search limit, then step towards
    /// the point `distance` metres behind the target's own yaw plus
    /// `angle` — `distance * sin( totalYaw )` on `x`, `cos` on `z` — and
    /// delegate the rest to [`EntityNavigate::navigate_step`], whose
    /// refusals and controller id pass through unchanged.
    #[allow(clippy::too_many_arguments)]
    pub fn navigate_follow(
        &mut self,
        state: &mut EntityState,
        navigator: &mut MeshNavigator,
        target: FollowTarget,
        angle: f32,
        distance: f32,
        velocity: f32,
        max_move_distance: f32,
        max_search_distance: f32,
        face_movement: bool,
        girth: f32,
        user_arg: i32,
        hertz: f32,
        max_aoi_radius: f32,
        next_controller_id: ControllerId,
        chunk_space: bool,
        now: u32,
    ) -> Result<ControllerId, NavigateFollowFailure> {
        // isEqual( maxSearchDistance, -1.f ) — resolved before every
        // check below, as in the other faces.
        let max_search_distance = if max_search_distance == -1.0 {
            max_aoi_radius
        } else {
            max_search_distance
        };

        // Check that the entity parameter actually has an entity
        if !target.is_entity {
            return Err(NavigateFollowFailure::NotEntity);
        }

        // A destroyed entity has no real location
        if target.destroyed {
            return Err(NavigateFollowFailure::Destroyed);
        }

        // Check that the entity is in the same space
        if !target.same_space {
            return Err(NavigateFollowFailure::DifferentSpace);
        }

        if almost_zero(max_search_distance) {
            return Err(NavigateFollowFailure::ZeroSearchDistance);
        }

        let total_yaw = target.yaw + angle;
        let offset = Vector3::new(distance * total_yaw.sin(), 0.0, distance * total_yaw.cos());
        let position = target.position + offset;

        self.navigate_step(
            state,
            navigator,
            position,
            velocity,
            max_move_distance,
            max_search_distance,
            face_movement,
            girth,
            user_arg,
            hertz,
            max_aoi_radius,
            next_controller_id,
            chunk_space,
            now,
        )
        .map_err(NavigateFollowFailure::NavigateStep)
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
            navigate_step: None,
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
            navigate_step: None,
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

    /// The navigateStep script refusals and the non-chunk-space face,
    /// with the reference's error texts.
    #[test]
    fn navigate_step_refuses_like_the_script_face() {
        let mut navigator = mesh_navigator("slope");
        let mut state = EntityState::new(7, Vector3::new(1.0, 0.5, 1.0), Direction3D::default(), 0);

        let mut ghost = EntityNavigate::default();
        let err = ghost
            .navigate_step(
                &mut state,
                &mut navigator,
                Vector3::new(7.0, 3.0, 7.0),
                2.0,
                100.0,
                -1.0,
                true,
                GIRTH,
                0,
                DEFAULT_GAME_UPDATE_HERTZ,
                MAX_AOI_RADIUS,
                4,
                true,
                1,
            )
            .expect_err("a ghost cannot navigate-step");
        assert_eq!(err, NavigateStepFailure::NotReal);
        assert_eq!(
            err.message(),
            "This method can only be called on a real entity."
        );

        let mut mounted = EntityNavigate {
            is_real: true,
            on_vehicle: true,
            navigation_system: NavmeshEntityNavigationSystem,
            navigate_step: None,
        };
        let err = mounted
            .navigate_step(
                &mut state,
                &mut navigator,
                Vector3::new(7.0, 3.0, 7.0),
                2.0,
                100.0,
                -1.0,
                true,
                GIRTH,
                0,
                DEFAULT_GAME_UPDATE_HERTZ,
                MAX_AOI_RADIUS,
                4,
                true,
                1,
            )
            .expect_err("a mounted entity cannot navigate-step");
        assert_eq!(err, NavigateStepFailure::OnVehicle);
        assert_eq!(
            err.message(),
            "Cannot call Entity.navigateStep() for an entity on a vehicle, either \
             alight from the vehicle first, or call Entity.navigateStep() for the \
             vehicle."
        );

        // A non-chunk (delegate) space refuses before any mesh work.
        let mut extra = real_entity();
        let err = extra
            .navigate_step(
                &mut state,
                &mut navigator,
                Vector3::new(7.0, 3.0, 7.0),
                2.0,
                100.0,
                -1.0,
                true,
                GIRTH,
                0,
                DEFAULT_GAME_UPDATE_HERTZ,
                MAX_AOI_RADIUS,
                4,
                false,
                1,
            )
            .expect_err("a delegate space cannot navigate");
        assert_eq!(err, NavigateStepFailure::NoSpace);
        assert_eq!(
            err.message(),
            "NavmeshEntityNavigationSystem::navigateStep(): \
             Physical space of entity does not support navigation queries"
        );
    }

    /// BW-10-06-02 acceptance — 单步目标、返回值与越界行为吻合: over the
    /// slope asset the wp1 → wp0 route is one waypoint transition, so
    /// `findPath` answers the destination navloc itself ("Use dst as
    /// point might not match", `navigator.cpp` `waypointsEqual( dst )`
    /// arm) — the single-step target is the whole destination; the
    /// answer is the controller id; the entity's `y` drops onto the step
    /// at once; the step controller then walks the leg straight and
    /// fires `onMove` on arrival.
    #[test]
    fn the_single_step_target_is_the_destination_for_a_one_hop_route() {
        let mut navigator = mesh_navigator("slope");
        let mut extra = real_entity();
        let mut state = EntityState::new(7, Vector3::new(1.0, 0.5, 1.0), Direction3D::default(), 0);

        let id = extra
            .navigate_step(
                &mut state,
                &mut navigator,
                Vector3::new(7.0, 3.0, 7.0),
                2.0,
                100.0,
                -1.0,
                true,
                GIRTH,
                42,
                DEFAULT_GAME_UPDATE_HERTZ,
                MAX_AOI_RADIUS,
                4,
                true,
                1,
            )
            .expect("the step resolves");
        assert_eq!(id, 4, "the controller id is the answer");

        let controller = extra.navigate_step.as_ref().expect("attached");
        assert_eq!(controller.destination, Vector3::new(7.0, 3.5, 7.0));
        assert!((controller.core.metres_per_tick - 0.2).abs() < 1e-5);
        // The entity's `y` is dropped onto the step; `x`/`z` stay until
        // the controller walks.
        assert_eq!(state.position, Vector3::new(1.0, 3.5, 1.0));

        // One 0.2 m hop per update; the leg arrives and `onMove` fires.
        // The script re-stepping nowhere new cancels the controller.
        let mut log = Vec::new();
        let mut now = 2u32;
        while !extra.navigate_step.as_mut().expect("attached").update(
            &mut state,
            now,
            &mut || None,
            &mut log,
        ) {
            now += 1;
            assert!(now < 200, "must arrive");
        }
        assert_eq!(state.position, Vector3::new(7.0, 3.5, 7.0));
        assert_eq!(
            log,
            vec![ScriptCallback {
                entity_id: 7,
                method: "onMove",
                controller_id: 4,
                user_arg: 42,
            }]
        );
    }

    /// A `maxMoveDistance` shorter than the leg trims the step along its
    /// `x`/`z` displacement; the trimmed point still projects into the
    /// source waypoint, so the height raise answers the source's max
    /// height (1.5) instead of the destination's 3.5.
    #[test]
    fn a_short_max_move_distance_trims_the_step() {
        let mut navigator = mesh_navigator("slope");
        let mut extra = real_entity();
        let mut state = EntityState::new(7, Vector3::new(1.0, 0.5, 1.0), Direction3D::default(), 0);

        extra
            .navigate_step(
                &mut state,
                &mut navigator,
                Vector3::new(7.0, 3.0, 7.0),
                2.0,
                1.0,
                -1.0,
                true,
                GIRTH,
                0,
                DEFAULT_GAME_UPDATE_HERTZ,
                MAX_AOI_RADIUS,
                4,
                true,
                1,
            )
            .expect("the step resolves");

        // The one-hop answer is (7, 3.5, 7): disp = (6, 0, 6),
        // wayDistance 8.49 > 1: the step is trimmed to exactly 1 m along
        // the diagonal.
        let controller = extra.navigate_step.as_ref().expect("attached");
        assert!(
            (controller.destination.x - (1.0 + 0.5f32.sqrt())).abs() < 1e-4
                && (controller.destination.z - (1.0 + 0.5f32.sqrt())).abs() < 1e-4,
            "trimmed to {:?}",
            controller.destination
        );
        assert!((controller.destination.y - 1.5).abs() < 1e-5);
        assert_eq!(state.position, Vector3::new(1.0, 1.5, 1.0));
    }

    /// An off-mesh source still resolves through the closest-waypoint
    /// fallback, so the step goes out from the far point; the trimmed
    /// point projects into nothing, so the height stays the found step's.
    #[test]
    fn an_off_mesh_source_still_steps_from_the_closest_waypoint() {
        let mut navigator = mesh_navigator("slope");
        let mut extra = real_entity();
        let mut state =
            EntityState::new(7, Vector3::new(50.0, 50.0, 50.0), Direction3D::default(), 0);

        extra
            .navigate_step(
                &mut state,
                &mut navigator,
                Vector3::new(7.0, 3.0, 7.0),
                2.0,
                1.0,
                -1.0,
                true,
                GIRTH,
                0,
                DEFAULT_GAME_UPDATE_HERTZ,
                MAX_AOI_RADIUS,
                4,
                true,
                1,
            )
            .expect("the closest fallback keeps the source valid");

        // The displacement is a 45-degree diagonal whichever hop the
        // search found, so the 1 m trim lands sqrt(0.5) per axis in; the
        // trimmed point projects into no waypoint and the height stays
        // the found step's 3.5.
        let controller = extra.navigate_step.as_ref().expect("attached");
        assert!(
            (controller.destination.x - (50.0 - 0.5f32.sqrt())).abs() < 1e-4
                && (controller.destination.z - (50.0 - 0.5f32.sqrt())).abs() < 1e-4,
            "stepped to {:?}",
            controller.destination
        );
        assert!((controller.destination.y - 3.5).abs() < 1e-5);
        assert_eq!(state.position, Vector3::new(50.0, 3.5, 50.0));
    }

    /// The unresolved faces: a girth no record matches fails the source
    /// first, the cross-layer shell route has no path, and a search limit
    /// at or below the space's grid size binds the in-set search while a
    /// larger one does not (the reference checks maxSearchDistance inside
    /// a navpoly set only when it is not greater than gridSize,
    /// `navigator.cpp:433-438`).
    #[test]
    fn the_unresolved_faces_answer_the_reference_errors() {
        let mut navigator = mesh_navigator("slope");
        let mut extra = real_entity();
        let mut state = EntityState::new(7, Vector3::new(1.0, 0.5, 1.0), Direction3D::default(), 0);

        let err = extra
            .navigate_step(
                &mut state,
                &mut navigator,
                Vector3::new(7.0, 3.0, 7.0),
                2.0,
                100.0,
                -1.0,
                true,
                0.5,
                0,
                DEFAULT_GAME_UPDATE_HERTZ,
                MAX_AOI_RADIUS,
                4,
                true,
                1,
            )
            .expect_err("the girth matches no record");
        assert_eq!(err, NavigateStepFailure::SourceUnresolved);
        assert_eq!(err.message(), "Could not resolve source position.");

        // The cross-layer shell route has no path.
        let mut shell = mesh_navigator("shell");
        let mut floor_state =
            EntityState::new(7, Vector3::new(2.5, 0.5, 4.5), Direction3D::default(), 0);
        let err = extra
            .navigate_step(
                &mut floor_state,
                &mut shell,
                Vector3::new(3.0, 1.5, 3.5),
                2.0,
                100.0,
                -1.0,
                true,
                GIRTH,
                0,
                DEFAULT_GAME_UPDATE_HERTZ,
                MAX_AOI_RADIUS,
                4,
                true,
                1,
            )
            .expect_err("the layers are disconnected");
        assert_eq!(err, NavigateStepFailure::NoPath);
        assert_eq!(err.message(), "No path found.");

        // A search limit at the grid size binds: the destination state
        // sits 8.9 m from the start, so the AStar skips it and no path is
        // found.
        state.position = Vector3::new(1.0, 0.5, 1.0);
        extra.navigate_step = None;
        let err = extra
            .navigate_step(
                &mut state,
                &mut navigator,
                Vector3::new(7.0, 3.0, 7.0),
                2.0,
                100.0,
                1.0,
                true,
                GIRTH,
                0,
                DEFAULT_GAME_UPDATE_HERTZ,
                MAX_AOI_RADIUS,
                4,
                true,
                1,
            )
            .expect_err("the search limit binds at the grid size");
        assert_eq!(err, NavigateStepFailure::NoPath);

        // One greater than the grid size does not bind the in-set search —
        // the 8.5 m route still resolves (this probe runs after the failed
        // one, whose search cleared the cache; a read/write findPath does
        // not re-validate a matched cache against the search limit).
        state.position = Vector3::new(1.0, 0.5, 1.0);
        extra.navigate_step = None;
        extra
            .navigate_step(
                &mut state,
                &mut navigator,
                Vector3::new(7.0, 3.0, 7.0),
                2.0,
                100.0,
                2.0,
                true,
                GIRTH,
                0,
                DEFAULT_GAME_UPDATE_HERTZ,
                MAX_AOI_RADIUS,
                4,
                true,
                1,
            )
            .expect("a limit above the grid size does not bind");
    }

    /// A second navigateStep retargets the attached controller and keeps
    /// its id and original `userArg` (the reuse arm ignores the new
    /// script arguments); the deregister hook clears it only for the
    /// matching id.
    #[test]
    fn a_second_call_reuses_the_registered_controller() {
        let mut navigator = mesh_navigator("slope");
        let mut extra = real_entity();
        let mut state = EntityState::new(7, Vector3::new(1.0, 0.5, 1.0), Direction3D::default(), 0);

        let id = extra
            .navigate_step(
                &mut state,
                &mut navigator,
                Vector3::new(7.0, 3.0, 7.0),
                2.0,
                100.0,
                -1.0,
                true,
                GIRTH,
                42,
                DEFAULT_GAME_UPDATE_HERTZ,
                MAX_AOI_RADIUS,
                4,
                true,
                1,
            )
            .expect("the step resolves");
        assert_eq!(id, 4);

        // Same inputs again: the attached controller is retargeted, not
        // replaced — id and userArg stay, the destination re-answers.
        state.position = Vector3::new(1.0, 0.5, 1.0);
        let id = extra
            .navigate_step(
                &mut state,
                &mut navigator,
                Vector3::new(7.0, 3.0, 7.0),
                3.0,
                100.0,
                -1.0,
                true,
                GIRTH,
                7,
                DEFAULT_GAME_UPDATE_HERTZ,
                MAX_AOI_RADIUS,
                9,
                true,
                2,
            )
            .expect("the second step resolves");
        assert_eq!(id, 4, "the attached controller is reused");
        let controller = extra.navigate_step.as_ref().expect("attached");
        assert_eq!(controller.destination, Vector3::new(7.0, 3.5, 7.0));
        assert_eq!(controller.user_arg, 42, "the reuse arm keeps the userArg");
        assert!((controller.core.metres_per_tick - 0.2).abs() < 1e-5);

        // deregister keeps a non-matching id and clears the match.
        extra.deregister_navigate_step_controller(9);
        assert!(extra.navigate_step.is_some());
        extra.deregister_navigate_step_controller(4);
        assert!(extra.navigate_step.is_none());

        // A fresh call after the deregister creates a new controller with
        // the id the caller offers.
        let id = extra
            .navigate_step(
                &mut state,
                &mut navigator,
                Vector3::new(7.0, 3.0, 7.0),
                2.0,
                100.0,
                -1.0,
                true,
                GIRTH,
                42,
                DEFAULT_GAME_UPDATE_HERTZ,
                MAX_AOI_RADIUS,
                9,
                true,
                3,
            )
            .expect("the fresh step resolves");
        assert_eq!(id, 9);
    }

    /// The canNavigateTo refusals: a ghost is refused, a non-chunk space is
    /// refused, and — unlike navigate/navigateStep — a mounted entity is
    /// **not** (the reference has no vehicle check on this face).
    #[test]
    fn can_navigate_to_refuses_the_non_real_face_but_not_a_mounted_entity() {
        let navigator = mesh_navigator("slope");
        let state = EntityState::new(7, Vector3::new(1.0, 0.5, 1.0), Direction3D::default(), 0);

        let ghost = EntityNavigate::default();
        let err = ghost
            .can_navigate_to(
                &state,
                &navigator,
                Vector3::new(7.0, 3.0, 7.0),
                -1.0,
                GIRTH,
                MAX_AOI_RADIUS,
                true,
            )
            .expect_err("a ghost cannot navigate");
        assert_eq!(err, CanNavigateFailure::NotReal);
        assert_eq!(
            err.message(),
            "This method can only be called on a real entity."
        );

        // A non-chunk (delegate) space refuses before any mesh work.
        let extra = real_entity();
        let err = extra
            .can_navigate_to(
                &state,
                &navigator,
                Vector3::new(7.0, 3.0, 7.0),
                -1.0,
                GIRTH,
                MAX_AOI_RADIUS,
                false,
            )
            .expect_err("a delegate space cannot navigate");
        assert_eq!(err, CanNavigateFailure::NoSpace);
        assert_eq!(
            err.message(),
            "NavmeshEntityNavigationSystem::canNavigateTo(): \
             Physical space of entity does not support navigation queries"
        );

        // The mounted entity walks straight through — no vehicle refusal
        // on this face.
        let mounted = EntityNavigate {
            is_real: true,
            on_vehicle: true,
            navigation_system: NavmeshEntityNavigationSystem,
            navigate_step: None,
        };
        let answer = mounted
            .can_navigate_to(
                &state,
                &navigator,
                Vector3::new(7.0, 3.0, 7.0),
                -1.0,
                GIRTH,
                MAX_AOI_RADIUS,
                true,
            )
            .expect("the reference has no vehicle check here");
        assert_eq!(answer, Some(Vector3::new(7.0, 3.5, 7.0)));
    }

    /// BW-10-06-03 acceptance (可达样本) — over the slope asset the
    /// reachable route answers the clipped destination; the read-only
    /// cache face still fills an empty cache and then answers a binding
    /// search limit from the hit (the local hit path ignores the limit),
    /// while a mismatched pair searches into a scratch cache and leaves
    /// the stored path untouched.
    #[test]
    fn a_reachable_destination_answers_the_clipped_point() {
        let mut navigator = mesh_navigator("slope");
        let extra = real_entity();

        // The same-waypoint shortcut needs no path search, so nothing is
        // cached.
        let state = EntityState::new(7, Vector3::new(1.0, 0.5, 1.0), Direction3D::default(), 0);
        let answer = extra
            .can_navigate_to(
                &state,
                &navigator,
                Vector3::new(2.0, 0.5, 2.0),
                -1.0,
                GIRTH,
                MAX_AOI_RADIUS,
                true,
            )
            .expect("the shortcut face");
        assert_eq!(answer, Some(Vector3::new(2.0, 1.5, 2.0)));
        assert!(navigator.cached_waypoint_path().is_empty());

        // The wp1 → wp0 route answers the clipped destination.
        let answer = extra
            .can_navigate_to(
                &state,
                &navigator,
                Vector3::new(7.0, 3.0, 7.0),
                -1.0,
                GIRTH,
                MAX_AOI_RADIUS,
                true,
            )
            .expect("the route resolves");
        assert_eq!(answer, Some(Vector3::new(7.0, 3.5, 7.0)));

        // ReadCache still fills an empty cache — only a *mismatched* one
        // is protected.
        let cached = navigator.cached_waypoint_path();
        assert!(!cached.is_empty());

        // The same pair with a search limit that binds a fresh search is
        // still answered — from the cache hit, which ignores the limit.
        let answer = extra
            .can_navigate_to(
                &state,
                &navigator,
                Vector3::new(7.0, 3.0, 7.0),
                1.0,
                GIRTH,
                MAX_AOI_RADIUS,
                true,
            )
            .expect("the cache hit ignores the limit");
        assert_eq!(answer, Some(Vector3::new(7.0, 3.5, 7.0)));

        // A mismatched pair searches into a scratch cache: the reverse
        // route answers, and the stored path stays exactly as it was.
        let reversed = EntityState::new(7, Vector3::new(7.0, 3.0, 7.0), Direction3D::default(), 0);
        let answer = extra
            .can_navigate_to(
                &reversed,
                &navigator,
                Vector3::new(1.0, 0.5, 1.0),
                -1.0,
                GIRTH,
                MAX_AOI_RADIUS,
                true,
            )
            .expect("the reverse route resolves");
        assert_eq!(answer, Some(Vector3::new(1.0, 1.5, 1.0)));
        assert_eq!(navigator.cached_waypoint_path(), cached);
    }

    /// BW-10-06-03 acceptance (不可达样本) — the cross-layer shell pair,
    /// an unmatched girth (the source answers None silently on this
    /// face), and a binding search limit with no cache to lean on all
    /// answer None.
    #[test]
    fn an_unreachable_destination_answers_none() {
        let extra = real_entity();

        // The shell's floor and bridge layers carry no inter-set adjacency.
        let shell = mesh_navigator("shell");
        let floor = EntityState::new(7, Vector3::new(2.5, 0.5, 4.5), Direction3D::default(), 0);
        let answer = extra
            .can_navigate_to(
                &floor,
                &shell,
                Vector3::new(3.0, 1.5, 3.5),
                -1.0,
                GIRTH,
                MAX_AOI_RADIUS,
                true,
            )
            .expect("the face itself works");
        assert_eq!(answer, None);

        // A girth no record matches: the source is unresolved and the
        // face answers None instead of raising.
        let slope = mesh_navigator("slope");
        let state = EntityState::new(7, Vector3::new(1.0, 0.5, 1.0), Direction3D::default(), 0);
        let answer = extra
            .can_navigate_to(
                &state,
                &slope,
                Vector3::new(7.0, 3.0, 7.0),
                -1.0,
                0.5,
                MAX_AOI_RADIUS,
                true,
            )
            .expect("the face itself works");
        assert_eq!(answer, None);

        // A binding search limit with no cache to lean on: the
        // destination state sits 8.9 m from the start, so the AStar skips
        // it.
        let answer = extra
            .can_navigate_to(
                &state,
                &slope,
                Vector3::new(7.0, 3.0, 7.0),
                1.0,
                GIRTH,
                MAX_AOI_RADIUS,
                true,
            )
            .expect("the face itself works");
        assert_eq!(answer, None);
    }

    /// A following target, in the same space, at `position` facing `yaw`.
    fn following_target(position: Vector3, yaw: f32) -> FollowTarget {
        FollowTarget {
            is_entity: true,
            destroyed: false,
            same_space: true,
            position,
            yaw,
        }
    }

    /// `navigateFollow` with the acceptance's defaults (velocity 2,
    /// maxMoveDistance 100, facing, the gen girth, userArg 42).
    #[allow(clippy::too_many_arguments)]
    fn follow(
        extra: &mut EntityNavigate,
        navigator: &mut MeshNavigator,
        state: &mut EntityState,
        target: FollowTarget,
        angle: f32,
        distance: f32,
        limit: f32,
        id: u16,
        now: u32,
    ) -> Result<ControllerId, NavigateFollowFailure> {
        extra.navigate_follow(
            state,
            navigator,
            target,
            angle,
            distance,
            2.0,
            100.0,
            limit,
            true,
            GIRTH,
            42,
            DEFAULT_GAME_UPDATE_HERTZ,
            MAX_AOI_RADIUS,
            id,
            true,
            now,
        )
    }

    /// The follow-specific refusals, in the reference's order: the target
    /// is checked for being an `Entity` before the search limit is
    /// checked for almost zero, then the delegated navigateStep
    /// refusals come through unchanged.
    #[test]
    fn navigate_follow_refuses_the_bad_target_and_the_zero_limit() {
        let mut navigator = mesh_navigator("slope");
        let mut extra = real_entity();
        let mut state = EntityState::new(7, Vector3::new(1.0, 0.5, 1.0), Direction3D::default(), 0);
        let target = following_target(Vector3::new(1.0, 0.5, 1.0), 0.0);

        // A non-Entity is refused before the limit check.
        let mut not_entity = target;
        not_entity.is_entity = false;
        let err = follow(
            &mut extra,
            &mut navigator,
            &mut state,
            not_entity,
            0.0,
            1.0,
            0.0,
            4,
            1,
        )
        .expect_err("parameter 1 must be an Entity instance");
        assert_eq!(err, NavigateFollowFailure::NotEntity);
        assert_eq!(err.message(), "parameter 1 must be an Entity instance");

        let mut destroyed = target;
        destroyed.destroyed = true;
        let err = follow(
            &mut extra,
            &mut navigator,
            &mut state,
            destroyed,
            0.0,
            1.0,
            -1.0,
            4,
            1,
        )
        .expect_err("a destroyed entity cannot be followed");
        assert_eq!(err, NavigateFollowFailure::Destroyed);
        assert_eq!(err.message(), "cannot follow a destroyed entity");

        let mut other_space = target;
        other_space.same_space = false;
        let err = follow(
            &mut extra,
            &mut navigator,
            &mut state,
            other_space,
            0.0,
            1.0,
            -1.0,
            4,
            1,
        )
        .expect_err("another space cannot be followed");
        assert_eq!(err, NavigateFollowFailure::DifferentSpace);
        assert_eq!(err.message(), "followed entity is not in the same space");

        // almostZero's default epsilon is 0.0004.
        let err = follow(
            &mut extra,
            &mut navigator,
            &mut state,
            target,
            0.0,
            1.0,
            0.0003,
            4,
            1,
        )
        .expect_err("the limit is refused inside the epsilon");
        assert_eq!(err, NavigateFollowFailure::ZeroSearchDistance);
        assert_eq!(err.message(), "maxSearchDistance cannot be 0.0");

        // Just outside the epsilon the limit passes the check and binds
        // the in-set search, so the delegated step finds no path. The
        // offset lands in the next waypoint's height band, so the
        // same-waypoint shortcut cannot answer instead.
        let err = follow(
            &mut extra,
            &mut navigator,
            &mut state,
            following_target(Vector3::new(1.0, 3.5, 1.0), 0.0),
            0.0,
            6.0,
            0.0005,
            4,
            1,
        )
        .expect_err("the limit binds the search just outside the epsilon");
        assert_eq!(
            err,
            NavigateFollowFailure::NavigateStep(NavigateStepFailure::NoPath)
        );
        assert_eq!(err.message(), NavigateStepFailure::NoPath.message());

        // The delegated step's own refusals pass through, and the entity
        // itself is checked last.
        let mut ghost = EntityNavigate::default();
        let err = follow(
            &mut ghost,
            &mut navigator,
            &mut state,
            target,
            0.0,
            1.0,
            -1.0,
            4,
            1,
        )
        .expect_err("a ghost cannot follow");
        assert_eq!(
            err,
            NavigateFollowFailure::NavigateStep(NavigateStepFailure::NotReal)
        );
        assert_eq!(
            err.message(),
            "This method can only be called on a real entity."
        );
    }

    /// BW-10-06-04 acceptance — 目标移动后跟随轨迹对照: the follower
    /// steps towards the point `distance` metres along the target's own
    /// yaw plus `angle`, and when the target moves a second call
    /// retargets the same controller to the new offset.
    #[test]
    fn a_following_entity_walks_the_offset_behind_the_target() {
        let mut navigator = mesh_navigator("slope");
        let mut extra = real_entity();
        let mut state = EntityState::new(7, Vector3::new(1.0, 0.5, 1.0), Direction3D::default(), 0);

        // The target sits at (1, 0.5, 1) facing yaw 0, so the point 1 m
        // along the offset is (1, 0.5, 2) — inside wp1.
        let id = follow(
            &mut extra,
            &mut navigator,
            &mut state,
            following_target(Vector3::new(1.0, 0.5, 1.0), 0.0),
            0.0,
            1.0,
            -1.0,
            4,
            1,
        )
        .expect("the follow resolves");
        assert_eq!(id, 4, "the controller id is the answer");
        let controller = extra.navigate_step.as_ref().expect("attached");
        assert_eq!(controller.destination, Vector3::new(1.0, 1.5, 2.0));
        assert_eq!(state.position, Vector3::new(1.0, 1.5, 1.0));

        let mut log = Vec::new();
        let mut now = 2u32;
        while !extra.navigate_step.as_mut().expect("attached").update(
            &mut state,
            now,
            &mut || None,
            &mut log,
        ) {
            now += 1;
            assert!(now < 200, "must arrive");
        }
        assert_eq!(state.position, Vector3::new(1.0, 1.5, 2.0));

        // The target moves: a second call retargets the attached
        // controller (same id, same userArg) to the new offset point
        // (1.5, 0.5, 2.5), and the follower walks the next leg.
        let id = follow(
            &mut extra,
            &mut navigator,
            &mut state,
            following_target(Vector3::new(1.5, 0.5, 1.5), 0.0),
            0.0,
            1.0,
            -1.0,
            9,
            now + 1,
        )
        .expect("the second follow resolves");
        assert_eq!(id, 4, "the attached controller is reused");
        let controller = extra.navigate_step.as_ref().expect("attached");
        assert_eq!(controller.destination, Vector3::new(1.5, 1.5, 2.5));
        assert_eq!(controller.user_arg, 42, "the reuse arm keeps the userArg");
        assert_eq!(
            state.position,
            Vector3::new(1.0, 1.5, 2.0),
            "the drop is a no-op in y"
        );

        let mut now = now + 1;
        while !extra.navigate_step.as_mut().expect("attached").update(
            &mut state,
            now,
            &mut || None,
            &mut log,
        ) {
            now += 1;
            assert!(now < 400, "must arrive");
        }
        assert_eq!(state.position, Vector3::new(1.5, 1.5, 2.5));
        assert_eq!(
            log,
            vec![
                ScriptCallback {
                    entity_id: 7,
                    method: "onMove",
                    controller_id: 4,
                    user_arg: 42,
                },
                ScriptCallback {
                    entity_id: 7,
                    method: "onMove",
                    controller_id: 4,
                    user_arg: 42,
                }
            ]
        );
    }

    /// The offset is built from the target's own yaw plus the script
    /// angle — `distance * sin( totalYaw )` on `x`, `cos` on `z` — so a
    /// target facing `pi` puts the follower on the other side of it.
    #[test]
    fn the_follow_offset_follows_the_target_yaw() {
        let mut navigator = mesh_navigator("slope");
        let mut extra = real_entity();
        let mut state = EntityState::new(7, Vector3::new(1.0, 0.5, 1.0), Direction3D::default(), 0);

        // yaw 0 + pi/2: the offset points along +x.
        follow(
            &mut extra,
            &mut navigator,
            &mut state,
            following_target(Vector3::new(1.0, 0.5, 1.0), 0.0),
            std::f32::consts::FRAC_PI_2,
            0.5,
            -1.0,
            4,
            1,
        )
        .expect("the follow resolves");
        let destination = extra.navigate_step.as_ref().expect("attached").destination;
        assert!(
            (destination.x - 1.5).abs() < 1e-6
                && (destination.y - 1.5).abs() < 1e-6
                && (destination.z - 1.0).abs() < 1e-6,
            "offset at {:?}",
            destination
        );
        extra.navigate_step = None;
        state.position = Vector3::new(1.0, 0.5, 1.0);

        // yaw pi + pi/2: the same script angle points along -x for a
        // target facing the other way.
        follow(
            &mut extra,
            &mut navigator,
            &mut state,
            following_target(Vector3::new(1.0, 0.5, 1.0), std::f32::consts::PI),
            std::f32::consts::FRAC_PI_2,
            0.5,
            -1.0,
            4,
            1,
        )
        .expect("the follow resolves");
        let destination = extra.navigate_step.as_ref().expect("attached").destination;
        assert!(
            (destination.x - 0.5).abs() < 1e-6
                && (destination.y - 1.5).abs() < 1e-6
                && (destination.z - 1.0).abs() < 1e-6,
            "offset at {:?}",
            destination
        );
    }
}
