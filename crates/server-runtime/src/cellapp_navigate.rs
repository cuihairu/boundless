//! The navigation family (`server/cellapp/navigation_controller.cpp`,
//! `move_controller.cpp`'s `NavigateStepController`, `entity_navigate.cpp`
//! and `navmesh_navigation_system.cpp`): the two exclusive `"Movement"`
//! controllers that walk the waypoint mesh, the per-entity `EntityNavigate`
//! extra that remembers the running navigate-step controller, and the
//! offload/restore stream the migration baseline rides on.
//!
//! The waypoint query stack (`lib/waypoint`'s `Navigator`/`NavLoc`/
//! `ChunkWaypointSet`, the A* itself) is the terrain lane's domain; this
//! module talks to it through the [`NavigationQueries`] seam — one trait
//! standing in for `Navigator` plus the mesh-touching `NavLoc` faces. An
//! entity with **no** navigator entry is the reference's
//! `!pChunkSpace()` face: every navigation query refuses, and a
//! `NavigationController::startReal` warns and hangs inert without ever
//! joining the updatables.
//!
//! `NavigateStepController::update` is the tick-budget loop: one update
//! spends a `tick = 1.0` budget through [`CellApp::move_towards`]'s
//! `remaining` parameter across however many waypoints the script's
//! `onMove` re-navigation chains, marking the destination invalid after
//! every arrival and cancelling when the script stops re-aiming.
//! Re-navigating to the same point a hundred times in one update is the
//! WARNING + `onMoveFailure` + cancel bail-out.
//!
//! The offload face (`Controllers::writeRealsToStream` /
//! `readRealsFromStream`): `lastAllocatedID_`, a count, then per
//! controller `id`, `type`, `userArg` and the kind's payload. The read
//! side creates without starting — `startRealControllers` re-arms the
//! drives (clamping stale timer starts, re-resolving navigation) once
//! the entity is real, and `callScriptInit` answers `onRestore` instead
//! of `__init__` on that face. Divergences kept and noted inline: the
//! reference's numeric `ControllerType` ids come from a link-order
//! static registry, so the port fixes its own stable codes; the
//! `isAttached` re-check inside `NavigationController::move` has no
//! port counterpart (nothing at this layer can cancel between the
//! position write and the completion test), so there is no
//! `NAVIGATION_CANCELLED` status; the loop-cap WARNING drops the
//! reference's `Space = %d` field (the port's entities carry no
//! space backlink at this layer); the pre-resolved `Exclusive`
//! placeholder streams its category id — a port-owned convention (the
//! reference has no such type).

use std::cell::RefCell;
use std::rc::Rc;

use foundation::math::{Vector3, almost_zero};
use foundation::stream::BinaryStream;

use crate::cellapp_real_entity::{AccelerationFacing, Controller, ControllerId, ControllerKind};
use crate::cellapp_space::{CellApp, Entity};

/// `stdmf.hpp:616`'s default `almostZero` band.
const ALMOST_ZERO_EPSILON: f32 = 0.0004;

/// The `reachedNextPos` band (`navigation_controller.cpp:136`).
const NEXT_POSITION_BAND: f32 = 0.001;

/// The invalid-destination sentinel (`FLT_MAX, FLT_MAX, FLT_MAX`).
pub const INVALID_DESTINATION: [f32; 3] = [f32::MAX, f32::MAX, f32::MAX];

/// `NavigationController::closeEnough_`'s constructor default — the value
/// the read face reconstructs (it is not on the stream).
const CLOSE_ENOUGH_DEFAULT: f32 = 0.01;

// The port's controller type codes. The reference's `ControllerType`
// numbers come from a static registry whose order depends on link order,
// so the port fixes its own stable table; round trips within the port
// are what the migration baseline checks.
const CONTROLLER_TYPE_TIMER: u32 = 1;
const CONTROLLER_TYPE_MOVE_TO_POINT: u32 = 2;
const CONTROLLER_TYPE_MOVE_TO_ENTITY: u32 = 3;
const CONTROLLER_TYPE_YAW_ROTATOR: u32 = 4;
const CONTROLLER_TYPE_ACCELERATE_TO_POINT: u32 = 5;
const CONTROLLER_TYPE_NAVIGATE_STEP: u32 = 6;
const CONTROLLER_TYPE_NAVIGATION: u32 = 7;
const CONTROLLER_TYPE_EXCLUSIVE: u32 = 8;

/// `NavLoc` (`lib/waypoint/navloc.hpp`) at the resolution this cluster
/// needs: which waypoint set (the port's stand-in for the
/// `ChunkWaypointSetPtr` identity), which waypoint in it, and the
/// resolved point. `set_id`/`waypoint` equality *is* the reference's
/// `waypointsEqual`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct NavLoc {
    /// The containing waypoint set's identity (`pSet_`).
    pub set_id: usize,
    /// The waypoint index (`waypoint_`); negative is "no single
    /// waypoint" (the closest-point resolution).
    pub waypoint: i32,
    /// The resolved point (`point_`).
    pub point: Vector3,
}

impl NavLoc {
    /// `NavLoc::waypointsEqual`: same set, same waypoint.
    pub fn waypoints_equal(&self, other: &NavLoc) -> bool {
        self.set_id == other.set_id && self.waypoint == other.waypoint
    }
}

/// The waypoint query stack seam: `Navigator` plus the mesh-touching
/// `NavLoc` faces, exactly the surface `navmesh_navigation_system.cpp`
/// and the two controllers use. The terrain lane owns the real
/// implementation; tests drive it with a scripted double.
pub trait NavigationQueries {
    /// `Navigator::createPathNavLoc` + `NavLoc::valid`: resolve a
    /// position onto the mesh, or `None` when off-mesh.
    fn create_path_nav_loc(&mut self, position: Vector3, girth: f32) -> Option<NavLoc>;

    /// `NavLoc::clip`: pull the point into its waypoint set.
    fn clip(&mut self, loc: &mut NavLoc);

    /// `NavLoc::makeMaxHeight`: raise the point onto the surface.
    fn make_max_height(&mut self, loc: &NavLoc, point: &mut Vector3);

    /// `NavLoc::containsProjection` (via the set's waypoint): whether
    /// the point still projects into the loc's waypoint.
    fn contains_projection(&mut self, loc: &NavLoc, point: Vector3) -> bool;

    /// `Navigator::findPath`: the next waypoint toward `dst` and whether
    /// an activated portal was passed.
    fn find_path(
        &mut self,
        src: &NavLoc,
        dst: &NavLoc,
        max_distance: f32,
    ) -> Option<(NavLoc, bool)>;

    /// `Navigator::getCachedWaypointPath`: the traversal path the last
    /// `findPath` left behind.
    fn cached_waypoint_path(&mut self) -> Vec<Vector3>;

    /// `Navigator::clearCachedWayPath`.
    fn clear_cached_way_path(&mut self);

    /// `Navigator::clearCachedWaySetPath`.
    fn clear_cached_way_set_path(&mut self);
}

/// A shared navigator handle (`CellApp` derives `Debug`, the trait
/// object does not).
#[derive(Clone)]
pub struct NavigatorHandle(pub Rc<RefCell<dyn NavigationQueries>>);

impl std::fmt::Debug for NavigatorHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("NavigatorHandle")
    }
}

/// `stdmf.hpp`'s bit-exact zero test (the movement module's `isZero`).
fn is_zero(value: f32) -> bool {
    value == 0.0
}

fn v3(components: [f32; 3]) -> Vector3 {
    Vector3::new(components[0], components[1], components[2])
}

/// `NavmeshEntityNavigationSystem::getNavigatePosition`
/// (`navmesh_navigation_system.cpp:550`): the next straight-line step
/// position between two valid navlocs. Same waypoint is the direct
/// answer raised onto the source's surface; otherwise the path search's
/// first waypoint with the surface height (`makeMaxHeight` from the
/// *way* loc — or the source when the way loc has no single waypoint)
/// never dropping below the way point's own y.
fn get_navigate_position(
    nav: &mut dyn NavigationQueries,
    src: &NavLoc,
    dst: &NavLoc,
    max_distance: f32,
) -> Option<(Vector3, bool)> {
    // if they are the same waypoint it is easy
    if src.waypoints_equal(dst) {
        let mut wp = dst.point;
        // set our y position now
        nav.make_max_height(src, &mut wp);
        return Some((wp, false));
    }
    let (way_loc, passed_activated_portal) = nav.find_path(src, dst, max_distance)?;
    // calculate the way world position, and ensure it is not too far away
    let mut wp = way_loc.point;
    let dst_y = wp.y;
    let height_loc = if way_loc.waypoint < 0 { *src } else { way_loc };
    nav.make_max_height(&height_loc, &mut wp);
    wp.y = wp.y.max(dst_y);
    Some((wp, passed_activated_portal))
}

/// `NavigationController::generateTraversalPath`
/// (`navigation_controller.cpp:368`): adopt the cached waypoint path,
/// aim at its first point, and append the destination itself when the
/// whole walk stays inside one waypoint set.
fn generate_traversal_path(
    nav: &mut dyn NavigationQueries,
    src: &NavLoc,
    dst: &NavLoc,
    destination: [f32; 3],
    path: &mut Vec<[f32; 3]>,
    next_position: &mut Vector3,
) {
    *path = nav
        .cached_waypoint_path()
        .into_iter()
        .map(|point| [point.x, point.y, point.z])
        .collect();
    if path.is_empty() {
        return;
    }
    *next_position = v3(path[0]);
    if src.set_id == dst.set_id {
        path.push(destination);
    }
}

/// The path refill inside `NavigationController::move`'s
/// `reachedNextPos` arm (`navigation_controller.cpp:147-180`): clear the
/// cached way path, re-resolve the source from the current position and
/// re-query the next position, then regenerate the traversal path.
#[allow(clippy::too_many_arguments)]
fn refill_navigation_path(
    nav: &mut dyn NavigationQueries,
    position: Vector3,
    girth: f32,
    max_distance: f32,
    destination: [f32; 3],
    dst_loc: &Option<NavLoc>,
    next_position: &mut Vector3,
    path: &mut Vec<[f32; 3]>,
) -> bool {
    nav.clear_cached_way_path();
    let Some(src_loc) = nav.create_path_nav_loc(position, girth) else {
        return false;
    };
    let Some(dst) = dst_loc.as_ref() else {
        return false;
    };
    let Some((resolved, _passed)) = get_navigate_position(nav, &src_loc, dst, max_distance) else {
        return false;
    };
    *next_position = resolved;
    path.clear();
    generate_traversal_path(nav, &src_loc, dst, destination, path, next_position);
    true
}

/// The navigate-step destination under `id`, or nothing when the
/// controller is gone or its slot now carries another family (the
/// exclusive id is shared across the whole movement category).
fn navigate_step_destination(
    entity_rc: &Rc<RefCell<Entity>>,
    id: ControllerId,
) -> Option<[f32; 3]> {
    entity_rc
        .borrow()
        .controllers
        .container
        .get(&id)
        .and_then(|controller| match &controller.kind {
            ControllerKind::NavigateStep { destination, .. } => Some(*destination),
            _ => None,
        })
}

/// `NavigateStepController::setDestination`: overwrite the stored
/// destination. Nothing between the caller's read and this write can
/// detach the controller.
fn set_navigate_step_destination(
    entity_rc: &Rc<RefCell<Entity>>,
    id: ControllerId,
    destination: [f32; 3],
) {
    let mut borrowed = entity_rc.borrow_mut();
    let slot = borrowed
        .controllers
        .container
        .get_mut(&id)
        .expect("nothing between the read and this write can detach the controller");
    if let ControllerKind::NavigateStep {
        destination: stored,
        ..
    } = &mut slot.kind
    {
        *stored = destination;
    }
}

/// The controller's user argument, or 0 when its slot is already gone.
fn controller_user_arg(entity_rc: &Rc<RefCell<Entity>>, id: ControllerId) -> i32 {
    entity_rc
        .borrow()
        .controllers
        .container
        .get(&id)
        .map(|controller| controller.user_arg)
        .unwrap_or(0)
}

/// `NavigationController::NavigationStatus`. The reference's
/// `NAVIGATION_CANCELLED` is omitted: nothing at this layer can cancel
/// the controller between its position write and the completion test.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum NavigationStatus {
    /// `NAVIGATION_FAILED`.
    Failed,
    /// `NAVIGATION_IN_PROGRESS`.
    InProgress,
    /// `NAVIGATION_COMPLETE`.
    Complete,
}

/// The port's stable type code of a kind (`Controller::type()`).
fn controller_type_code(kind: &ControllerKind) -> u32 {
    match kind {
        ControllerKind::Timer { .. } => CONTROLLER_TYPE_TIMER,
        ControllerKind::Exclusive { .. } => CONTROLLER_TYPE_EXCLUSIVE,
        ControllerKind::MoveToPoint { .. } => CONTROLLER_TYPE_MOVE_TO_POINT,
        ControllerKind::MoveToEntity { .. } => CONTROLLER_TYPE_MOVE_TO_ENTITY,
        ControllerKind::YawRotator { .. } => CONTROLLER_TYPE_YAW_ROTATOR,
        ControllerKind::AccelerateToPoint { .. } => CONTROLLER_TYPE_ACCELERATE_TO_POINT,
        ControllerKind::NavigateStep { .. } => CONTROLLER_TYPE_NAVIGATE_STEP,
        ControllerKind::Navigation { .. } => CONTROLLER_TYPE_NAVIGATION,
    }
}

/// One kind's `writeRealToStream` payload, in the reference's field
/// order. `entity_velocity` feeds the acceleration family's
/// `currVelocity_` (this port keeps that state on the entity).
fn write_controller_payload(
    stream: &mut BinaryStream,
    kind: &ControllerKind,
    entity_velocity: [f32; 3],
) {
    match kind {
        ControllerKind::Timer { start, interval } => {
            stream.write_u32(*start).write_u32(*interval);
        }
        // The port's pre-resolved placeholder streams its category id —
        // a port-owned convention (the reference has no such type).
        ControllerKind::Exclusive { category } => {
            stream.write_u32(*category as u32);
        }
        ControllerKind::MoveToPoint {
            destination,
            metres_per_tick,
            face_movement,
            move_vertically,
        } => {
            stream.write_f32(*metres_per_tick);
            stream.write_bool(*face_movement);
            stream.write_bool(*move_vertically);
            write_position(stream, *destination);
            // The script factory's hardcoded destinationChunk "" and
            // waypoint 0.
            let _ = stream.write_string("");
            stream.write_i32(0);
        }
        ControllerKind::MoveToEntity {
            target,
            range,
            metres_per_tick,
            face_movement,
            move_vertically,
            ..
        } => {
            stream.write_f32(*metres_per_tick);
            stream.write_bool(*face_movement);
            stream.write_bool(*move_vertically);
            stream.write_i32(*target as i32);
            stream.write_f32(*range);
        }
        ControllerKind::YawRotator {
            target_yaw,
            radians_per_tick,
        } => {
            stream.write_f32(*target_yaw).write_f32(*radians_per_tick);
        }
        ControllerKind::AccelerateToPoint {
            destination,
            acceleration,
            max_speed,
            facing,
            stop_at_destination,
        } => {
            stream.write_f32(*acceleration);
            stream.write_f32(*max_speed);
            stream.write_i32(match facing {
                AccelerationFacing::None => 0,
                AccelerationFacing::Velocity => 1,
                AccelerationFacing::Acceleration => 2,
            });
            write_position(stream, entity_velocity);
            write_position(stream, *destination);
            stream.write_bool(*stop_at_destination);
        }
        ControllerKind::NavigateStep {
            destination,
            metres_per_tick,
            face_movement,
            move_vertically,
        } => {
            stream.write_f32(*metres_per_tick);
            stream.write_bool(*face_movement);
            stream.write_bool(*move_vertically);
            write_position(stream, *destination);
        }
        ControllerKind::Navigation {
            destination,
            metres_per_tick,
            face_movement,
            max_distance,
            girth,
            ..
        } => {
            write_position(stream, *destination);
            stream.write_f32(*metres_per_tick);
            stream.write_bool(*face_movement);
            stream.write_f32(*max_distance);
            stream.write_f32(*girth);
        }
    }
}

/// One kind's `readRealFromStream` payload. `None` is a malformed stream
/// face (an unknown type code or an out-of-range facing).
fn read_controller_payload(stream: &mut BinaryStream, type_code: u32) -> Option<ControllerKind> {
    let kind = match type_code {
        CONTROLLER_TYPE_TIMER => ControllerKind::Timer {
            start: stream.read_u32().ok()?,
            interval: stream.read_u32().ok()?,
        },
        CONTROLLER_TYPE_EXCLUSIVE => ControllerKind::Exclusive {
            category: stream.read_u32().ok()? as ControllerId,
        },
        CONTROLLER_TYPE_MOVE_TO_POINT => {
            let metres_per_tick = stream.read_f32().ok()?;
            let face_movement = stream.read_bool().ok()?;
            let move_vertically = stream.read_bool().ok()?;
            let destination = read_position(stream)?;
            let _chunk = stream.read_string().ok()?;
            let _waypoint = stream.read_i32().ok()?;
            ControllerKind::MoveToPoint {
                destination,
                metres_per_tick,
                face_movement,
                move_vertically,
            }
        }
        CONTROLLER_TYPE_MOVE_TO_ENTITY => {
            let metres_per_tick = stream.read_f32().ok()?;
            let face_movement = stream.read_bool().ok()?;
            let move_vertically = stream.read_bool().ok()?;
            let target = stream.read_i32().ok()? as u32;
            let range = stream.read_f32().ok()?;
            // `pDestEntity_` starts NULL after a read: the resolve and
            // the offset draw happen again on the first update.
            ControllerKind::MoveToEntity {
                target,
                range,
                metres_per_tick,
                face_movement,
                move_vertically,
                offset: [0.0, 0.0],
                resolved: false,
            }
        }
        CONTROLLER_TYPE_YAW_ROTATOR => ControllerKind::YawRotator {
            target_yaw: stream.read_f32().ok()?,
            radians_per_tick: stream.read_f32().ok()?,
        },
        CONTROLLER_TYPE_ACCELERATE_TO_POINT => {
            let acceleration = stream.read_f32().ok()?;
            let max_speed = stream.read_f32().ok()?;
            let facing = match stream.read_i32().ok()? {
                0 => AccelerationFacing::None,
                1 => AccelerationFacing::Velocity,
                2 => AccelerationFacing::Acceleration,
                _ => return None,
            };
            // `currVelocity_` is read and dropped: this port keeps the
            // sampled velocity on the entity, which the entity restore
            // carries.
            let _current_velocity = read_position(stream)?;
            let destination = read_position(stream)?;
            let stop_at_destination = stream.read_bool().ok()?;
            ControllerKind::AccelerateToPoint {
                destination,
                acceleration,
                max_speed,
                facing,
                stop_at_destination,
            }
        }
        CONTROLLER_TYPE_NAVIGATE_STEP => {
            let metres_per_tick = stream.read_f32().ok()?;
            let face_movement = stream.read_bool().ok()?;
            let move_vertically = stream.read_bool().ok()?;
            let destination = read_position(stream)?;
            ControllerKind::NavigateStep {
                destination,
                metres_per_tick,
                face_movement,
                move_vertically,
            }
        }
        CONTROLLER_TYPE_NAVIGATION => {
            let destination = read_position(stream)?;
            let metres_per_tick = stream.read_f32().ok()?;
            let face_movement = stream.read_bool().ok()?;
            let max_distance = stream.read_f32().ok()?;
            let girth = stream.read_f32().ok()?;
            // `NavigationController::readRealFromStream`:
            // nextPosition_ = destination_; the traversal state is
            // startReal's to rebuild, and closeEnough_ keeps the
            // constructor default (it is not on the stream).
            ControllerKind::Navigation {
                destination,
                metres_per_tick,
                face_movement,
                max_distance,
                girth,
                close_enough: CLOSE_ENOUGH_DEFAULT,
                next_position: destination,
                path: Vec::new(),
                current_node: 0,
                dst_loc: None,
            }
        }
        _ => return None,
    };
    Some(kind)
}

fn write_position(stream: &mut BinaryStream, position: [f32; 3]) {
    stream.write_f32(position[0]);
    stream.write_f32(position[1]);
    stream.write_f32(position[2]);
}

fn read_position(stream: &mut BinaryStream) -> Option<[f32; 3]> {
    Some([
        stream.read_f32().ok()?,
        stream.read_f32().ok()?,
        stream.read_f32().ok()?,
    ])
}

impl CellApp {
    /// `EntityNavigate::instance`'s slot: the extra type's eeid,
    /// registered on first use the way the reference's static
    /// `Instance<EntityNavigate>` does.
    pub fn navigate_extra_eeid(&mut self) -> usize {
        if let Some(index) = self
            .extra_types
            .iter()
            .position(|name| *name == "EntityNavigate")
        {
            return index;
        }
        self.register_entity_extra("EntityNavigate")
    }

    /// `NavigateStepController::startReal`'s registration half: touch the
    /// `EntityNavigate` extra (lazily constructing it) and store the
    /// controller pointer.
    fn register_navigate_step_controller(&mut self, entity: u32, id: ControllerId) {
        let eeid = self.navigate_extra_eeid();
        self.touch_extra(entity, eeid);
        self.navigate_step_controllers.insert(entity, id);
    }

    /// `EntityNavigate::isRealToScript`: the entity is live and real.
    fn is_real_to_script(&self, entity: u32) -> bool {
        self.entity_population.get(&entity).is_some_and(|rc| {
            let borrowed = rc.borrow();
            borrowed.is_real && !borrowed.destroyed
        })
    }

    /// `EntityNavigate::navigateStep` +
    /// `NavmeshEntityNavigationSystem::navigateStep`
    /// (`navmesh_navigation_system.cpp:317`): resolve both ends, query
    /// the next hop, land its height immediately, and aim the registered
    /// navigate-step controller at it — attaching one when the extra
    /// holds no pointer. `None` is a refusal, already on the
    /// [`CellApp::script_errors`] ledger.
    #[allow(clippy::too_many_arguments)]
    pub fn navigate_step(
        &mut self,
        entity: u32,
        dst_position: [f32; 3],
        velocity: f32,
        max_move_distance: f32,
        max_search_distance: f32,
        face_movement: bool,
        girth: f32,
        user_arg: i32,
    ) -> Option<ControllerId> {
        let max_search_distance = if max_search_distance == -1.0 {
            self.max_aoi_radius
        } else {
            max_search_distance
        };
        if !self.is_real_to_script(entity) {
            // The vehicle refusal is BW-08-02-05's face.
            self.script_errors
                .push((entity, "This method can only be called on a real entity."));
            return None;
        }
        let entity_rc = self.entity_population.get(&entity).cloned()?;
        let Some(navigator) = self.navigators.get(&entity).cloned() else {
            self.script_errors.push((
                entity,
                "NavmeshEntityNavigationSystem::navigateStep(): Physical space of entity does not support navigation queries",
            ));
            return None;
        };
        let position = v3(entity_rc.borrow().position);
        let mut nav = navigator.0.borrow_mut();
        let ends = (
            nav.create_path_nav_loc(position, girth),
            nav.create_path_nav_loc(v3(dst_position), girth),
        );
        let (src_loc, mut dst_loc) = match ends {
            (Some(src), Some(dst)) => (src, dst),
            (None, _) => {
                self.script_errors
                    .push((entity, "Could not resolve source position."));
                return None;
            }
            (_, None) => {
                self.script_errors
                    .push((entity, "Could not resolve destination position."));
                return None;
            }
        };
        nav.clip(&mut dst_loc);

        let Some((mut next_position, _passed)) =
            get_navigate_position(&mut *nav, &src_loc, &dst_loc, max_search_distance)
        else {
            self.script_errors.push((entity, "No path found."));
            return None;
        };

        let mut disp = next_position - position;
        disp.y = 0.0;
        let way_distance = disp.length();
        let mut dst_y = next_position.y;

        // calculate the way world position at the source's surface height
        let mut wsv = next_position;
        nav.make_max_height(&src_loc, &mut wsv);
        let src_y = wsv.y;

        if way_distance > max_move_distance {
            // shorten it
            let mut shortened = disp;
            shortened.scale(max_move_distance / way_distance);
            next_position = position + shortened;
            if nav.contains_projection(&src_loc, next_position) {
                dst_y = src_y;
            }
        }

        // set our y position now
        next_position.y = dst_y.max(src_y);
        let mut landed = position;
        landed.y = next_position.y;
        let direction = v3(entity_rc.borrow().direction);
        // Not on a vehicle, so the global position/direction face is the
        // plain one.
        self.set_entity_position_and_direction(&entity_rc, landed, direction);
        drop(nav);

        let registered = self.navigate_step_controllers.get(&entity).copied();
        let live = registered.is_some_and(|pid| {
            entity_rc
                .borrow()
                .controllers
                .container
                .get(&pid)
                .is_some_and(|controller| {
                    matches!(controller.kind, ControllerKind::NavigateStep { .. })
                })
        });
        if live {
            let id = registered.expect("live was resolved from the same option");
            set_navigate_step_destination(
                &entity_rc,
                id,
                [next_position.x, next_position.y, next_position.z],
            );
            Some(id)
        } else {
            let controller = Controller {
                kind: ControllerKind::navigate_step(
                    [next_position.x, next_position.y, next_position.z],
                    velocity,
                    face_movement,
                    self.update_hertz,
                ),
                user_arg,
            };
            Some(self.attach_controller(entity, controller))
        }
    }

    /// `EntityNavigate::navigate` (`entity_navigate.cpp:597`) +
    /// `NavmeshEntityNavigationSystem::createController`: construct the
    /// full-path `NavigationController` — the `-1` search distance
    /// becomes `maxAoIRadius` and a sub-milli `closeEnough` draws the
    /// constructor WARNING — and attach it, which runs `startReal`'s
    /// resolution. `None` is the ghost refusal.
    #[allow(clippy::too_many_arguments)]
    pub fn navigate(
        &mut self,
        entity: u32,
        destination: [f32; 3],
        velocity: f32,
        face_movement: bool,
        max_search_distance: f32,
        girth: f32,
        close_enough: f32,
        user_arg: i32,
    ) -> Option<ControllerId> {
        let max_search_distance = if max_search_distance == -1.0 {
            self.max_aoi_radius
        } else {
            max_search_distance
        };
        if !self.is_real_to_script(entity) {
            // The vehicle refusal is BW-08-02-05's face.
            self.script_errors
                .push((entity, "This method can only be called on a real entity."));
            return None;
        }
        if close_enough < 0.001 {
            self.navigation_warnings.push((
                entity,
                format!(
                    "NavigationController::NavigationController: closeEnough_ is very small ({close_enough})"
                ),
            ));
        }
        let controller = Controller {
            kind: ControllerKind::navigation(
                destination,
                velocity,
                face_movement,
                max_search_distance,
                girth,
                close_enough,
                self.update_hertz,
                self.max_aoi_radius,
            ),
            user_arg,
        };
        Some(self.attach_controller(entity, controller))
    }

    /// `EntityNavigate::canNavigateTo` +
    /// `NavmeshEntityNavigationSystem::canNavigateTo`: the closest
    /// on-mesh position the entity can navigate to, or `None` when no
    /// path exists (the reference's `Py_RETURN_NONE` faces).
    pub fn can_navigate_to(
        &mut self,
        entity: u32,
        dst_position: [f32; 3],
        max_search_distance: f32,
        girth: f32,
    ) -> Option<[f32; 3]> {
        let max_search_distance = if max_search_distance == -1.0 {
            self.max_aoi_radius
        } else {
            max_search_distance
        };
        if !self.is_real_to_script(entity) {
            self.script_errors
                .push((entity, "This method can only be called on a real entity."));
            return None;
        }
        let navigator = self.navigators.get(&entity).cloned()?;
        let position = v3(self.entity_population.get(&entity)?.borrow().position);
        let mut nav = navigator.0.borrow_mut();
        let ends = (
            nav.create_path_nav_loc(position, girth),
            nav.create_path_nav_loc(v3(dst_position), girth),
        );
        let (src_loc, mut dst_loc) = match ends {
            (Some(src), Some(dst)) => (src, dst),
            _ => return None,
        };
        nav.clip(&mut dst_loc);
        if src_loc.waypoints_equal(&dst_loc) {
            let mut wp = dst_loc.point;
            nav.make_max_height(&src_loc, &mut wp);
            return Some([wp.x, wp.y, wp.z]);
        }
        nav.find_path(&src_loc, &dst_loc, max_search_distance)?;
        let mut wp = dst_loc.point;
        nav.make_max_height(&dst_loc, &mut wp);
        Some([wp.x, wp.y, wp.z])
    }

    /// `NavigateStepController::startReal` (`move_controller.cpp:314`):
    /// the `MoveController` registration plus the `EntityNavigate`
    /// pointer registration.
    pub(crate) fn start_navigate_step_controller(
        &mut self,
        entity_rc: &Rc<RefCell<Entity>>,
        id: ControllerId,
        kind: ControllerKind,
    ) {
        self.start_movement_controller(entity_rc, id, kind);
        let entity = entity_rc.borrow().id;
        self.register_navigate_step_controller(entity, id);
    }

    /// `NavigationController::startReal` (`navigation_controller.cpp:220`):
    /// resolve the source and destination navlocs — clipping the
    /// destination, whose point becomes the stored destination — query
    /// the first leg, adopt the traversal path, and join the updatables.
    /// An unresolvable end leaves `nextPosition_` at the entity position
    /// (and `pDstLoc_` unresolved, which fails the drive's refills).
    /// Without a navigator the reference warns and returns before the
    /// registration — the controller hangs inert.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn start_full_navigation_controller(
        &mut self,
        entity_rc: &Rc<RefCell<Entity>>,
        id: ControllerId,
        close_enough: f32,
        destination: [f32; 3],
        metres_per_tick: f32,
        face_movement: bool,
        max_distance: f32,
        girth: f32,
    ) {
        let entity = entity_rc.borrow().id;
        let Some(navigator) = self.navigators.get(&entity).cloned() else {
            self.navigation_warnings.push((
                entity,
                "NavigationController::startReal: Physical Space type of entity is not supported"
                    .to_string(),
            ));
            return;
        };
        let position = v3(entity_rc.borrow().position);

        // currentNode_ = 0; path_.clear();
        let mut next_position = position;
        let mut path: Vec<[f32; 3]> = Vec::new();
        let mut stored_destination = destination;
        let mut dst_slot: Option<NavLoc> = None;

        let mut nav = navigator.0.borrow_mut();
        let ends = (
            nav.create_path_nav_loc(position, girth),
            nav.create_path_nav_loc(v3(destination), girth),
        );
        if let (Some(src), Some(mut dst)) = ends {
            nav.clip(&mut dst);
            stored_destination = [dst.point.x, dst.point.y, dst.point.z];
            nav.clear_cached_way_path();
            nav.clear_cached_way_set_path();
            match get_navigate_position(&mut *nav, &src, &dst, max_distance) {
                Some((leg, _passed)) => {
                    next_position = leg;
                    generate_traversal_path(
                        &mut *nav,
                        &src,
                        &dst,
                        stored_destination,
                        &mut path,
                        &mut next_position,
                    );
                }
                None => next_position = position,
            }
            dst_slot = Some(dst);
        }
        drop(nav);

        let mut borrowed = entity_rc.borrow_mut();
        let slot = borrowed
            .controllers
            .container
            .get_mut(&id)
            .expect("the navigation controller was just attached");
        slot.kind = ControllerKind::Navigation {
            destination: stored_destination,
            metres_per_tick,
            face_movement,
            max_distance,
            girth,
            close_enough,
            next_position: [next_position.x, next_position.y, next_position.z],
            path,
            current_node: 0,
            dst_loc: dst_slot,
        };
        drop(borrowed);
        self.register_updatable(entity, id);
    }

    /// `NavigateStepController::update` (`move_controller.cpp:371`): the
    /// tick-budget loop. Every arrival marks the destination invalid and
    /// fires `onMove`; the script's re-navigation (through
    /// [`CellApp::navigate_step`]) re-aims it and the loop carries the
    /// unspent budget to the next hop; a script that stops re-aiming
    /// leaves the invalid destination in place, and the loop cancels. A
    /// hundred loops in one update is the WARNING + `onMoveFailure` +
    /// cancel bail-out.
    pub(crate) fn update_navigate_step(
        &mut self,
        entity_rc: &Rc<RefCell<Entity>>,
        entity: u32,
        id: ControllerId,
        metres_per_tick: f32,
        face_movement: bool,
        move_vertically: bool,
    ) {
        let user_arg = controller_user_arg(entity_rc, id);
        let mut tick = 1.0f32;
        let mut loops: i32 = 0;
        loop {
            if almost_zero(tick, ALMOST_ZERO_EPSILON) {
                break;
            }
            let Some(destination) = navigate_step_destination(entity_rc, id) else {
                break; // !isAttached, or the family swapped under the id
            };
            let moved = self.move_towards(
                entity_rc,
                metres_per_tick,
                face_movement,
                move_vertically,
                v3(destination),
                Some(&mut tick),
            );
            if !moved {
                break;
            }
            set_navigate_step_destination(entity_rc, id, INVALID_DESTINATION);
            loops += 1;
            if loops >= 100 {
                let position = entity_rc.borrow().position;
                self.navigation_warnings.push((
                    entity,
                    format!(
                        "NavigateStepController::update: Entity {}: destination = ({:.2}, {:.2}, {:.2}). numOfLoops = {} and tick still {:.3}. This is likely caused by script navigating to the same point in onMove callback. Quitting loop.",
                        entity, position[0], position[1], position[2], loops, tick
                    ),
                ));
                self.entity_callback(entity, "onMoveFailure", false, Some((id, user_arg)));
                if entity_rc.borrow().controllers.container.contains_key(&id) {
                    self.cancel_controller(entity, id);
                }
                return;
            }
            self.entity_callback(entity, "onMove", false, Some((id, user_arg)));
            if navigate_step_destination(entity_rc, id) == Some(INVALID_DESTINATION) {
                self.cancel_controller(entity, id);
            }
        }
    }

    /// `NavigationController::update` (`navigation_controller.cpp:205`):
    /// step the walk, then the standard callback and self-cancel on
    /// completion or failure.
    pub(crate) fn update_navigation_controller(
        &mut self,
        entity_rc: &Rc<RefCell<Entity>>,
        entity: u32,
        id: ControllerId,
    ) {
        let status = self.navigation_move(entity_rc, id);
        let user_arg = controller_user_arg(entity_rc, id);
        let name = match status {
            NavigationStatus::Complete => "onNavigate",
            NavigationStatus::Failed => "onNavigateFailed",
            NavigationStatus::InProgress => return,
        };
        self.entity_callback(entity, name, false, Some((id, user_arg)));
        if entity_rc.borrow().controllers.container.contains_key(&id) {
            self.cancel_controller(entity, id);
        }
    }

    /// `NavigationController::move` (`navigation_controller.cpp:70`): the
    /// walk step. Arrival on the final destination completes; arrival on
    /// the next position advances the traversal path, and an exhausted
    /// path refills it from a fresh source resolution.
    fn navigation_move(
        &mut self,
        entity_rc: &Rc<RefCell<Entity>>,
        id: ControllerId,
    ) -> NavigationStatus {
        let entity = entity_rc.borrow().id;
        let Some(ControllerKind::Navigation {
            destination,
            metres_per_tick,
            face_movement,
            max_distance,
            girth,
            close_enough,
            next_position: stored_next,
            path,
            current_node,
            dst_loc,
        }) = entity_rc
            .borrow()
            .controllers
            .container
            .get(&id)
            .map(|controller| controller.kind.clone())
        else {
            // The dispatch only routes the navigation family here; a
            // different family under the id (or a missing controller)
            // leaves the walk where it is.
            return NavigationStatus::InProgress;
        };
        let mut next_position = v3(stored_next);
        let mut path = path;
        let mut current_node = current_node;
        let (mut position, mut direction) = {
            let borrowed = entity_rc.borrow();
            (v3(borrowed.position), v3(borrowed.direction))
        };

        let mut movement = next_position - position;
        if movement.y < 0.0 {
            movement.y = 0.0;
        }

        // If we are real close, then finish up.
        if movement.length() < metres_per_tick {
            position = next_position;
        } else {
            movement.normalise();
            movement.scale(metres_per_tick);
            position = position + movement;
            if position.y < next_position.y {
                position.y = next_position.y;
            }
        }

        // Make sure we are facing the right direction.
        if face_movement && (!is_zero(movement.x) || !is_zero(movement.z)) {
            direction.x = movement.yaw();
        }

        // No longer on the ground...
        entity_rc.borrow_mut().is_on_ground = false;
        self.set_entity_position_and_direction(entity_rc, position, direction);

        // Check for completion
        if (position.x - destination[0]).abs() < close_enough
            && (position.z - destination[2]).abs() < close_enough
        {
            self.write_navigation_state(
                entity_rc,
                id,
                destination,
                next_position,
                path,
                current_node,
                dst_loc,
            );
            return NavigationStatus::Complete;
        }

        // .. otherwise check if we are at the next update location
        let reached_next = (position.x - next_position.x).abs() < NEXT_POSITION_BAND
            && (position.z - next_position.z).abs() < NEXT_POSITION_BAND;
        if reached_next {
            if current_node < path.len() {
                next_position = v3(path[current_node]);
                current_node += 1;
                self.write_navigation_state(
                    entity_rc,
                    id,
                    destination,
                    next_position,
                    path,
                    current_node,
                    dst_loc,
                );
                return NavigationStatus::InProgress;
            }
            let Some(navigator) = self.navigators.get(&entity).cloned() else {
                // The nav system died with the entity's extra; the walk
                // coasts on its stored state until something else ends
                // it.
                self.write_navigation_state(
                    entity_rc,
                    id,
                    destination,
                    next_position,
                    path,
                    current_node,
                    dst_loc,
                );
                return NavigationStatus::InProgress;
            };
            let mut nav = navigator.0.borrow_mut();
            let refilled = refill_navigation_path(
                &mut *nav,
                position,
                girth,
                max_distance,
                destination,
                &dst_loc,
                &mut next_position,
                &mut path,
            );
            drop(nav);
            if !refilled {
                self.write_navigation_state(
                    entity_rc,
                    id,
                    destination,
                    next_position,
                    path,
                    current_node,
                    dst_loc,
                );
                return NavigationStatus::Failed;
            }
            // If we are still navigating, setup the state for the next
            // round of movement.
            current_node = 0;
        }
        self.write_navigation_state(
            entity_rc,
            id,
            destination,
            next_position,
            path,
            current_node,
            dst_loc,
        );
        NavigationStatus::InProgress
    }

    /// Write the navigation controller's runtime state back.
    #[allow(clippy::too_many_arguments)]
    fn write_navigation_state(
        &self,
        entity_rc: &Rc<RefCell<Entity>>,
        id: ControllerId,
        destination: [f32; 3],
        next_position: Vector3,
        path: Vec<[f32; 3]>,
        current_node: usize,
        dst_loc: Option<NavLoc>,
    ) {
        let mut borrowed = entity_rc.borrow_mut();
        let Some(slot) = borrowed.controllers.container.get_mut(&id) else {
            return;
        };
        let ControllerKind::Navigation {
            destination: slot_destination,
            next_position: slot_next,
            path: slot_path,
            current_node: slot_node,
            dst_loc: slot_dst,
            ..
        } = &mut slot.kind
        else {
            return;
        };
        *slot_destination = destination;
        *slot_next = [next_position.x, next_position.y, next_position.z];
        *slot_path = path;
        *slot_node = current_node;
        *slot_dst = dst_loc;
    }

    /// `Controllers::writeRealsToStream` (`controllers.cpp:174`): the
    /// offload block — `lastAllocatedID_`, the count of real-domain
    /// controllers, then per controller (in id order, the reference's
    /// map iteration) `id`, `type`, `userArg` and the payload.
    pub fn write_real_controllers_to_stream(&self, entity: u32) -> Option<Vec<u8>> {
        let entity_rc = self.entity_population.get(&entity)?;
        let borrowed = entity_rc.borrow();
        let mut stream = BinaryStream::new();
        stream.write_u32(borrowed.controllers.last_allocated_id as u32);
        stream.write_u32(borrowed.controllers.container.len() as u32);
        let velocity = borrowed.velocity;
        for (cid, controller) in &borrowed.controllers.container {
            stream.write_u32(*cid as u32);
            stream.write_u32(controller_type_code(&controller.kind));
            stream.write_i32(controller.user_arg);
            write_controller_payload(&mut stream, &controller.kind, velocity);
        }
        Some(stream.as_bytes().to_vec())
    }

    /// `Controllers::readRealsFromStream` (`controllers.cpp:82`): the
    /// restore half — recreate the controllers without starting them
    /// (`startRealControllers` re-arms once the entity is real). The
    /// reference's restore-into-ghost branch cannot fire here: this
    /// port's entities carry no ghost controllers (the ghost domain is
    /// a later cluster's face).
    pub fn read_real_controllers_from_stream(&mut self, entity: u32, data: &[u8]) -> bool {
        let Some(entity_rc) = self.entity_population.get(&entity).cloned() else {
            return false;
        };
        let mut stream = BinaryStream::from_slice(data);
        let Ok(last_allocated) = stream.read_u32() else {
            return false;
        };
        let Ok(count) = stream.read_u32() else {
            return false;
        };
        entity_rc.borrow_mut().controllers.last_allocated_id = last_allocated as ControllerId;
        for _ in 0..count {
            let Ok(cid) = stream.read_u32() else {
                return false;
            };
            debug_assert!(
                !entity_rc
                    .borrow()
                    .controllers
                    .container
                    .contains_key(&(cid as ControllerId)),
                "restoring into a live controller is the ghost cluster's face"
            );
            let Ok(type_code) = stream.read_u32() else {
                return false;
            };
            let Ok(user_arg) = stream.read_i32() else {
                return false;
            };
            let Some(kind) = read_controller_payload(&mut stream, type_code) else {
                return false;
            };
            entity_rc
                .borrow_mut()
                .controllers
                .container
                .insert(cid as ControllerId, Controller { kind, user_arg });
        }
        true
    }
}

/// `NavigateStepController::stopReal`'s deregistration
/// (`entity_navigate.cpp:93`): only the controller the extra points at
/// clears the pointer.
pub(crate) fn deregister_navigate_step(app: &mut CellApp, entity: u32, id: ControllerId) {
    if app.navigate_step_controllers.get(&entity) == Some(&id) {
        app.navigate_step_controllers.remove(&entity);
    }
}

// ---------------------------------------------------------------------------
// Section: tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cellapp_real_entity::{
        ADD_ENTITY_DATA_IS_ON_GROUND, CreateRealOutcome, EntityScript, MethodAction,
    };
    use crate::cellapp_space::Space;
    use crate::cellappmgr::SpaceId;
    use std::collections::{BTreeMap, BTreeSet, VecDeque};

    const SID: SpaceId = 3;
    const TYPE: u32 = 7;
    const MOVEMENT: ControllerId = 0x8000;

    /// A scripted waypoint query stack: queued answers that repeat their
    /// last entry once the queue runs dry, plus the call counters.
    #[derive(Default)]
    struct ScriptedNavigator {
        resolve_answers: VecDeque<Option<NavLoc>>,
        last_resolve: Option<Option<NavLoc>>,
        find_path_answers: VecDeque<Option<(NavLoc, bool)>>,
        last_find_path: Option<Option<(NavLoc, bool)>>,
        cached_paths: VecDeque<Vec<Vector3>>,
        last_cached_path: Vec<Vector3>,
        clip_to: Option<Vector3>,
        max_height_y: Option<f32>,
        contains_projection: bool,
        /// Shared with the test so the count survives the trait-object
        /// handle.
        clear_way_path_counter: Rc<RefCell<u32>>,
        clear_way_set_calls: u32,
    }

    impl ScriptedNavigator {
        /// A navigator that answers `count` resolutions of the same
        /// loc (the direct same-waypoint walk).
        fn flat(point: [f32; 3], count: usize, max_height_y: Option<f32>) -> Self {
            let mut nav = Self::default();
            for _ in 0..count {
                nav.resolve_answers.push_back(Some(loc(1, 0, point)));
            }
            nav.max_height_y = max_height_y;
            nav
        }
    }

    /// The queue's front answer, or the last one once drained.
    fn next_answer<T: Clone>(queue: &mut VecDeque<T>, last: &mut Option<T>) -> Option<T> {
        if let Some(answer) = queue.pop_front() {
            *last = Some(answer.clone());
            return Some(answer);
        }
        last.clone()
    }

    impl NavigationQueries for ScriptedNavigator {
        fn create_path_nav_loc(&mut self, _position: Vector3, _girth: f32) -> Option<NavLoc> {
            next_answer(&mut self.resolve_answers, &mut self.last_resolve).flatten()
        }

        fn clip(&mut self, loc: &mut NavLoc) {
            if let Some(point) = self.clip_to {
                loc.point = point;
            }
        }

        fn make_max_height(&mut self, _loc: &NavLoc, point: &mut Vector3) {
            if let Some(y) = self.max_height_y {
                point.y = point.y.max(y);
            }
        }

        fn contains_projection(&mut self, _loc: &NavLoc, _point: Vector3) -> bool {
            self.contains_projection
        }

        fn find_path(
            &mut self,
            _src: &NavLoc,
            _dst: &NavLoc,
            _max_distance: f32,
        ) -> Option<(NavLoc, bool)> {
            next_answer(&mut self.find_path_answers, &mut self.last_find_path).flatten()
        }

        fn cached_waypoint_path(&mut self) -> Vec<Vector3> {
            match self.cached_paths.pop_front() {
                Some(path) => {
                    self.last_cached_path = path.clone();
                    path
                }
                None => self.last_cached_path.clone(),
            }
        }

        fn clear_cached_way_path(&mut self) {
            *self.clear_way_path_counter.borrow_mut() += 1;
        }

        fn clear_cached_way_set_path(&mut self) {
            self.clear_way_set_calls += 1;
        }
    }

    fn loc(set_id: usize, waypoint: i32, point: [f32; 3]) -> NavLoc {
        NavLoc {
            set_id,
            waypoint,
            point: v3(point),
        }
    }

    /// The creation stream at a chosen position.
    fn create_stream(id: u32, type_id: u32, position: [f32; 3]) -> Vec<u8> {
        let mut stream = Vec::new();
        stream.extend_from_slice(&id.to_le_bytes());
        stream.extend_from_slice(&type_id.to_le_bytes());
        for value in position {
            stream.extend_from_slice(&value.to_le_bytes());
        }
        stream.push(ADD_ENTITY_DATA_IS_ON_GROUND);
        for value in [0.0f32, 0.0, 0.0] {
            stream.extend_from_slice(&value.to_le_bytes());
        }
        stream.extend_from_slice(&42u32.to_le_bytes());
        stream.extend_from_slice(&7u32.to_le_bytes());
        stream.extend_from_slice(&6502u16.to_le_bytes());
        stream.push(b'-');
        stream
    }

    fn spaces_with() -> BTreeMap<SpaceId, Space> {
        let mut spaces = BTreeMap::new();
        spaces.insert(SID, Space::new(SID));
        spaces
    }

    /// An app whose fixture type defines the given methods.
    fn app_with(script: EntityScript) -> CellApp {
        let mut app = CellApp::default();
        app.entity_scripts.insert(TYPE, script);
        app
    }

    fn plain_script(extra: &[&'static str]) -> EntityScript {
        let mut script = EntityScript::with_init(Vec::new());
        for &name in extra {
            script.methods.insert(name);
        }
        script
    }

    fn create(
        app: &mut CellApp,
        spaces: &mut BTreeMap<SpaceId, Space>,
        id: u32,
        position: [f32; 3],
    ) {
        let outcome = app.create_real_entity(spaces, SID, &create_stream(id, TYPE, position));
        assert!(matches!(outcome, CreateRealOutcome::Created(_)));
    }

    fn entity_position(app: &CellApp, id: u32) -> [f32; 3] {
        app.entity_population[&id].borrow().position
    }

    /// Asserts that exactly `count` controllers restored and one of
    /// them answers `predicate`. The ids come from the allocator (not
    /// the shared movement category), so the assertion scans the
    /// container instead of a fixed id.
    fn assert_restored_kind<F>(restored: &CellApp, count: usize, predicate: F)
    where
        F: Fn(&ControllerKind) -> bool,
    {
        let kinds = restored.entity_population[&77]
            .borrow()
            .controllers
            .container
            .values()
            .map(|controller| controller.kind.clone())
            .collect::<Vec<_>>();
        assert_eq!(kinds.len(), count);
        assert!(kinds.iter().any(&predicate), "{kinds:?}");
    }

    #[test]
    fn navigate_step_lands_the_height_and_attaches_a_controller() {
        let mut app = app_with(plain_script(&["onMove"]));
        let mut spaces = spaces_with();
        create(&mut app, &mut spaces, 77, [0.0, 0.0, 0.0]);

        // A ghost (is_real false) draws the isRealToScript TypeError
        // before anything else.
        app.entity_population.get(&77).unwrap().borrow_mut().is_real = false;
        assert_eq!(
            app.navigate_step(77, [3.0, 0.0, 0.0], 10.0, 100.0, -1.0, true, 0.5, 9),
            None
        );
        assert_eq!(
            app.script_errors[0].1,
            "This method can only be called on a real entity."
        );
        app.entity_population.get(&77).unwrap().borrow_mut().is_real = true;

        // The way point sits at [3, 4, 0] with a surface height of 2.
        app.navigators.insert(
            77,
            NavigatorHandle(Rc::new(RefCell::new(ScriptedNavigator::flat(
                [3.0, 4.0, 0.0],
                8,
                Some(2.0),
            )))),
        );
        // The handle carries no debug surface of its own.
        assert_eq!(format!("{:?}", app.navigators[&77]), "NavigatorHandle");

        // An explicit search distance passes through untouched.
        let id = app.navigate_step(77, [3.0, 0.0, 0.0], 10.0, 100.0, 600.0, true, 0.5, 9);
        assert_eq!(id, Some(MOVEMENT));

        // The y face: the next hop's height is max(dstY, srcY) = 4 (the
        // way point) over the surface's 2, and the entity's y lands on
        // it immediately while x/z stay.
        assert_eq!(entity_position(&app, 77), [0.0, 4.0, 0.0]);
        // The controller is registered on the extra's pointer and in the
        // updatables, aimed at the hop.
        assert_eq!(app.navigate_step_controllers.get(&77), Some(&MOVEMENT));
        let borrowed = app.entity_population[&77].borrow();
        assert!(matches!(
            &borrowed.controllers.container[&MOVEMENT].kind,
            ControllerKind::NavigateStep {
                destination: [3.0, 4.0, 0.0],
                metres_per_tick,
                ..
            } if (metres_per_tick - 1.0).abs() < 1e-6 // velocity 10 / hertz 10
        ));
        assert!(app.updatable_controllers.contains(&Some((77, MOVEMENT))));
        // The EntityNavigate extra was lazily constructed.
        drop(borrowed);
        let eeid = app.navigate_extra_eeid();
        assert!(app.extra_exists(77, eeid));
    }

    #[test]
    fn navigate_step_without_a_navigator_is_refused() {
        let mut app = app_with(plain_script(&[]));
        let mut spaces = spaces_with();
        create(&mut app, &mut spaces, 77, [0.0, 0.0, 0.0]);

        assert_eq!(
            app.navigate_step(77, [1.0, 0.0, 0.0], 10.0, 100.0, -1.0, true, 0.5, 0),
            None
        );
        assert_eq!(
            app.script_errors[0].1,
            "NavmeshEntityNavigationSystem::navigateStep(): Physical space of entity does not support navigation queries"
        );
    }

    #[test]
    fn navigate_step_refuses_offmesh_ends_and_missing_paths() {
        let mut app = app_with(plain_script(&[]));
        let mut spaces = spaces_with();
        create(&mut app, &mut spaces, 77, [0.0, 0.0, 0.0]);

        // Off-mesh source.
        let mut nav = ScriptedNavigator::default();
        nav.resolve_answers.push_back(None);
        app.navigators
            .insert(77, NavigatorHandle(Rc::new(RefCell::new(nav))));
        assert_eq!(
            app.navigate_step(77, [1.0, 0.0, 0.0], 10.0, 100.0, -1.0, true, 0.5, 0),
            None
        );
        assert_eq!(app.script_errors[0].1, "Could not resolve source position.");

        // Off-mesh destination.
        let mut app2 = app_with(plain_script(&[]));
        let mut spaces2 = spaces_with();
        create(&mut app2, &mut spaces2, 77, [0.0, 0.0, 0.0]);
        let mut nav = ScriptedNavigator::default();
        nav.resolve_answers
            .push_back(Some(loc(1, 0, [0.0, 0.0, 0.0])));
        nav.resolve_answers.push_back(None);
        app2.navigators
            .insert(77, NavigatorHandle(Rc::new(RefCell::new(nav))));
        assert_eq!(
            app2.navigate_step(77, [1.0, 0.0, 0.0], 10.0, 100.0, -1.0, true, 0.5, 0),
            None
        );
        assert_eq!(
            app2.script_errors[0].1,
            "Could not resolve destination position."
        );

        // No path between the ends.
        let mut app3 = app_with(plain_script(&[]));
        let mut spaces3 = spaces_with();
        create(&mut app3, &mut spaces3, 77, [0.0, 0.0, 0.0]);
        let mut nav = ScriptedNavigator::default();
        nav.resolve_answers
            .push_back(Some(loc(1, 0, [0.0, 0.0, 0.0])));
        nav.resolve_answers
            .push_back(Some(loc(2, 0, [5.0, 0.0, 0.0])));
        nav.find_path_answers.push_back(None);
        app3.navigators
            .insert(77, NavigatorHandle(Rc::new(RefCell::new(nav))));
        assert_eq!(
            app3.navigate_step(77, [5.0, 0.0, 0.0], 10.0, 100.0, -1.0, true, 0.5, 0),
            None
        );
        assert_eq!(app3.script_errors[0].1, "No path found.");
    }

    #[test]
    fn navigate_step_shortens_hops_past_the_max_move_distance() {
        let mut app = app_with(plain_script(&["onMove"]));
        let mut spaces = spaces_with();
        create(&mut app, &mut spaces, 77, [0.0, 0.0, 0.0]);
        // The way point sits 10 away with a surface height of 1; the hop
        // must shorten to the 2-metre cap and — the projection still
        // inside the source waypoint — take the source's height.
        let mut nav = ScriptedNavigator::flat([10.0, 1.0, 0.0], 4, Some(1.0));
        nav.contains_projection = true;
        app.navigators
            .insert(77, NavigatorHandle(Rc::new(RefCell::new(nav))));

        let id = app.navigate_step(77, [10.0, 0.0, 0.0], 10.0, 2.0, -1.0, true, 0.5, 0);
        assert_eq!(id, Some(MOVEMENT));
        // x/z stay at the entity's spot; only the height lands (on the
        // source's 1.0, dstY having been replaced by srcY).
        assert_eq!(entity_position(&app, 77), [0.0, 1.0, 0.0]);
        // The controller walks toward the shortened hop.
        let borrowed = app.entity_population[&77].borrow();
        assert!(matches!(
            &borrowed.controllers.container[&MOVEMENT].kind,
            ControllerKind::NavigateStep {
                destination: [2.0, 1.0, 0.0],
                ..
            }
        ));

        // Outside the source waypoint's projection the way point's own
        // height survives the shorten (dstY untouched).
        let mut app2 = app_with(plain_script(&["onMove"]));
        let mut spaces2 = spaces_with();
        create(&mut app2, &mut spaces2, 77, [0.0, 0.0, 0.0]);
        let nav = ScriptedNavigator::flat([10.0, 3.0, 0.0], 4, Some(1.0));
        app2.navigators
            .insert(77, NavigatorHandle(Rc::new(RefCell::new(nav))));
        app2.navigate_step(77, [10.0, 0.0, 0.0], 10.0, 2.0, -1.0, true, 0.5, 0);
        assert_eq!(entity_position(&app2, 77), [0.0, 3.0, 0.0]);
        let borrowed = app2.entity_population[&77].borrow();
        assert!(matches!(
            &borrowed.controllers.container[&MOVEMENT].kind,
            ControllerKind::NavigateStep {
                destination: [2.0, 3.0, 0.0],
                ..
            }
        ));
    }

    #[test]
    fn navigate_step_on_a_registered_controller_re_aims_it() {
        let mut app = app_with(plain_script(&["onMove"]));
        let mut spaces = spaces_with();
        create(&mut app, &mut spaces, 77, [0.0, 0.0, 0.0]);
        app.navigators.insert(
            77,
            NavigatorHandle(Rc::new(RefCell::new(ScriptedNavigator::flat(
                [1.0, 0.0, 0.0],
                8,
                None,
            )))),
        );

        let first = app.navigate_step(77, [1.0, 0.0, 0.0], 10.0, 100.0, -1.0, true, 0.5, 9);
        assert_eq!(first, Some(MOVEMENT));

        // A stale pointer (its controller already cancelled) attaches a
        // fresh one instead of re-aiming a corpse.
        app.cancel_controller(77, MOVEMENT);
        assert!(!app.navigate_step_controllers.contains_key(&77));

        let second = app.navigate_step(77, [1.0, 0.0, 0.0], 10.0, 100.0, -1.0, true, 0.5, 9);
        assert_eq!(second, Some(MOVEMENT));

        // The live pointer is a re-aim, not a new attach: no second
        // controller exists and the original user arg survives.
        let before = app.script_calls.len();
        let third = app.navigate_step(77, [1.0, 0.0, 0.0], 10.0, 100.0, -1.0, true, 0.5, 5);
        assert_eq!(third, Some(MOVEMENT));
        assert_eq!(app.script_calls.len(), before);
        let borrowed = app.entity_population[&77].borrow();
        assert_eq!(borrowed.controllers.container.len(), 1);
        assert_eq!(borrowed.controllers.container[&MOVEMENT].user_arg, 9);
    }

    #[test]
    fn a_navigate_step_arrival_without_renavigation_cancels() {
        let mut app = app_with(plain_script(&["onMove"]));
        let mut spaces = spaces_with();
        create(&mut app, &mut spaces, 77, [0.0, 0.0, 0.0]);
        // The hop is 0.5 away; metresPerTick is 1, so the first update
        // arrives on it.
        app.navigators.insert(
            77,
            NavigatorHandle(Rc::new(RefCell::new(ScriptedNavigator::flat(
                [0.5, 0.0, 0.0],
                4,
                None,
            )))),
        );
        app.navigate_step(77, [0.5, 0.0, 0.0], 10.0, 100.0, -1.0, true, 0.5, 4);

        app.advance_tick();

        // Arrived verbatim, onMove fired with the user arg (after the
        // creation-time __init__), and the controller cancelled itself
        // (the script never re-aimed).
        assert_eq!(entity_position(&app, 77), [0.5, 0.0, 0.0]);
        assert_eq!(app.script_calls.len(), 2);
        assert_eq!(app.script_calls[0].name, "__init__");
        assert_eq!(app.script_calls[1].name, "onMove");
        assert_eq!(app.script_calls[1].timer, Some((MOVEMENT, 4)));
        assert!(
            app.entity_population[&77]
                .borrow()
                .controllers
                .container
                .is_empty()
        );
        assert!(!app.navigate_step_controllers.contains_key(&77));
        assert!(app.updatable_controllers.is_empty());
    }

    #[test]
    fn re_navigation_chains_hops_on_one_budget() {
        // The script's onMove re-navigates; the double's queued answers
        // walk the aim from [0.6] to [1.6]. The first hop arrives inside
        // the initial tick and leaves 0.4 of budget, which the next
        // iteration spends walking 0.4 of the metre — the budget
        // carries across the re-aimed hop.
        let mut script = plain_script(&["onMove"]);
        script
            .method_actions
            .entry("onMove")
            .or_default()
            .push(MethodAction::NavigateStep {
                destination: [10.0, 0.0, 0.0],
                velocity: 10.0,
                max_move_distance: 100.0,
            });
        let mut app = app_with(script);
        let mut spaces = spaces_with();
        create(&mut app, &mut spaces, 77, [0.0, 0.0, 0.0]);
        let mut nav = ScriptedNavigator::default();
        for point in [
            [0.6f32, 0.0, 0.0],
            [0.6, 0.0, 0.0],
            [1.6, 0.0, 0.0],
            [1.6, 0.0, 0.0],
        ] {
            nav.resolve_answers.push_back(Some(loc(1, 0, point)));
        }
        // The second update's onMove re-navigation loses the mesh: the
        // source resolve answers None, the aim stays invalid and the
        // controller cancels.
        nav.resolve_answers.push_back(None);
        app.navigators
            .insert(77, NavigatorHandle(Rc::new(RefCell::new(nav))));
        app.navigate_step(77, [10.0, 0.0, 0.0], 10.0, 100.0, -1.0, true, 0.5, 0);

        app.advance_tick();

        // Hop one arrived, onMove re-aimed at [1.6], and the leftover
        // 0.4 budget walked 0.4 of the metre before the loop ran dry.
        let position = entity_position(&app, 77);
        assert!((position[0] - 1.0).abs() < 1e-5);
        assert_eq!(app.script_calls.len(), 2); // __init__ + onMove
        assert_eq!(app.script_calls[1].name, "onMove");
        let borrowed = app.entity_population[&77].borrow();
        assert!(matches!(
            &borrowed.controllers.container[&MOVEMENT].kind,
            ControllerKind::NavigateStep {
                destination: [1.6, 0.0, 0.0],
                ..
            }
        ));
        drop(borrowed);

        // The next update finishes the hop on a fresh budget; the
        // script's failed re-navigation then leaves the invalid
        // destination in place and the controller cancels.
        app.advance_tick();
        assert_eq!(entity_position(&app, 77), [1.6, 0.0, 0.0]);
        assert_eq!(app.script_calls.len(), 3); // __init__ + 2 onMove
        assert!(
            app.entity_population[&77]
                .borrow()
                .controllers
                .container
                .is_empty()
        );
        assert_eq!(app.script_errors[0].1, "Could not resolve source position.");
    }

    #[test]
    fn same_point_re_navigation_hits_the_loop_cap() {
        // onMove re-navigates to the very point the entity stands on:
        // every iteration arrives without spending budget, so the 100th
        // loop is the WARNING + onMoveFailure + cancel face.
        let mut script = plain_script(&["onMove", "onMoveFailure"]);
        script
            .method_actions
            .entry("onMove")
            .or_default()
            .push(MethodAction::NavigateStep {
                destination: [0.0, 0.0, 0.0],
                velocity: 10.0,
                max_move_distance: 100.0,
            });
        let mut app = app_with(script);
        let mut spaces = spaces_with();
        create(&mut app, &mut spaces, 77, [0.0, 0.0, 0.0]);
        app.navigators.insert(
            77,
            NavigatorHandle(Rc::new(RefCell::new(ScriptedNavigator::flat(
                [0.0, 0.0, 0.0],
                8,
                None,
            )))),
        );
        app.navigate_step(77, [0.0, 0.0, 0.0], 10.0, 100.0, -1.0, true, 0.5, 0);

        app.advance_tick();

        // Ninety-nine onMove calls, then the failure face.
        let on_moves = app
            .script_calls
            .iter()
            .filter(|call| call.name == "onMove")
            .count();
        assert_eq!(on_moves, 99);
        let failure = app
            .script_calls
            .iter()
            .find(|call| call.name == "onMoveFailure")
            .expect("the loop cap fires onMoveFailure");
        assert_eq!(failure.timer, Some((MOVEMENT, 0)));
        assert_eq!(app.navigation_warnings.len(), 1);
        assert!(app.navigation_warnings[0].1.contains("Quitting loop."));
        assert!(
            app.entity_population[&77]
                .borrow()
                .controllers
                .container
                .is_empty()
        );
    }

    #[test]
    fn a_decay_of_the_same_hop_drains_the_budget() {
        // Re-aiming at a point that is not almost zero spends only the
        // fraction of the budget the last walk used, so the standing
        // aim decays 1.0 → 0.6 → 0.36 … until the loop-top
        // almostZero check closes the loop with the aim intact.
        let mut script = plain_script(&["onMove"]);
        script
            .method_actions
            .entry("onMove")
            .or_default()
            .push(MethodAction::NavigateStep {
                destination: [0.4, 0.0, 0.0],
                velocity: 10.0,
                max_move_distance: 100.0,
            });
        let mut app = app_with(script);
        let mut spaces = spaces_with();
        create(&mut app, &mut spaces, 77, [0.0, 0.0, 0.0]);
        app.navigators.insert(
            77,
            NavigatorHandle(Rc::new(RefCell::new(ScriptedNavigator::flat(
                [0.4, 0.0, 0.0],
                16,
                None,
            )))),
        );
        app.navigate_step(77, [0.4, 0.0, 0.0], 10.0, 100.0, -1.0, true, 0.5, 0);

        app.advance_tick();

        assert_eq!(entity_position(&app, 77), [0.4, 0.0, 0.0]);
        // __init__ plus one onMove per decay step.
        let on_moves = app
            .script_calls
            .iter()
            .filter(|call| call.name == "onMove")
            .count();
        assert_eq!(on_moves, 5);
        assert!(app.navigation_warnings.is_empty());
        let borrowed = app.entity_population[&77].borrow();
        assert!(matches!(
            &borrowed.controllers.container[&MOVEMENT].kind,
            ControllerKind::NavigateStep {
                destination: [0.4, 0.0, 0.0],
                ..
            }
        ));
    }

    #[test]
    fn a_script_cancel_inside_on_move_ends_the_walk() {
        let mut script = plain_script(&["onMove"]);
        script
            .method_actions
            .entry("onMove")
            .or_default()
            .push(MethodAction::CancelMovement);
        let mut app = app_with(script);
        let mut spaces = spaces_with();
        create(&mut app, &mut spaces, 77, [0.0, 0.0, 0.0]);
        app.navigators.insert(
            77,
            NavigatorHandle(Rc::new(RefCell::new(ScriptedNavigator::flat(
                [0.5, 0.0, 0.0],
                4,
                None,
            )))),
        );
        app.navigate_step(77, [0.5, 0.0, 0.0], 10.0, 100.0, -1.0, true, 0.5, 0);

        app.advance_tick();

        // The arrival fires onMove, the script's cancel takes the
        // controller down, and the loop exits without a second cancel
        // attempt.
        assert_eq!(app.script_calls.len(), 2); // __init__ + onMove
        assert_eq!(app.script_calls[1].name, "onMove");
        assert!(
            app.entity_population[&77]
                .borrow()
                .controllers
                .container
                .is_empty()
        );
        assert!(!app.navigate_step_controllers.contains_key(&77));
    }

    #[test]
    fn a_script_cancel_on_move_failure_skips_the_second_cancel() {
        // The loop-cap's onMoveFailure callback cancels the controller
        // itself: the post-callback cancel-if-attached guard must skip.
        let mut script = plain_script(&["onMove", "onMoveFailure"]);
        script
            .method_actions
            .entry("onMove")
            .or_default()
            .push(MethodAction::NavigateStep {
                destination: [0.0, 0.0, 0.0],
                velocity: 10.0,
                max_move_distance: 100.0,
            });
        script
            .method_actions
            .entry("onMoveFailure")
            .or_default()
            .push(MethodAction::CancelMovement);
        let mut app = app_with(script);
        let mut spaces = spaces_with();
        create(&mut app, &mut spaces, 77, [0.0, 0.0, 0.0]);
        app.navigators.insert(
            77,
            NavigatorHandle(Rc::new(RefCell::new(ScriptedNavigator::flat(
                [0.0, 0.0, 0.0],
                8,
                None,
            )))),
        );
        app.navigate_step(77, [0.0, 0.0, 0.0], 10.0, 100.0, -1.0, true, 0.5, 0);

        app.advance_tick();

        assert!(
            app.script_calls
                .iter()
                .any(|call| call.name == "onMoveFailure")
        );
        assert_eq!(app.navigation_warnings.len(), 1);
        assert!(
            app.entity_population[&77]
                .borrow()
                .controllers
                .container
                .is_empty()
        );
    }

    #[test]
    fn the_navigate_step_drive_tolerates_a_swapped_family_and_a_lost_slot() {
        let mut app = app_with(plain_script(&[]));
        let mut spaces = spaces_with();
        create(&mut app, &mut spaces, 77, [0.0, 0.0, 0.0]);
        app.navigators.insert(
            77,
            NavigatorHandle(Rc::new(RefCell::new(ScriptedNavigator::flat(
                [0.5, 0.0, 0.0],
                4,
                None,
            )))),
        );
        app.navigate_step(77, [0.5, 0.0, 0.0], 10.0, 100.0, -1.0, true, 0.5, 0);
        let entity_rc = app.entity_population[&77].clone();

        // White-box: another family under the shared exclusive id makes
        // the drive read answer nothing and the loop exit.
        entity_rc.borrow_mut().controllers.container.insert(
            MOVEMENT,
            Controller {
                kind: ControllerKind::MoveToPoint {
                    destination: [9.0, 0.0, 0.0],
                    metres_per_tick: 0.0,
                    face_movement: false,
                    move_vertically: false,
                },
                user_arg: 0,
            },
        );
        app.update_navigate_step(&entity_rc, 77, MOVEMENT, 1.0, true, false);
        assert_eq!(entity_position(&app, 77), [0.0, 0.0, 0.0]);
        // A missing controller answers the same way.
        app.update_navigate_step(&entity_rc, 77, 0x1997, 1.0, true, false);
        assert_eq!(entity_position(&app, 77), [0.0, 0.0, 0.0]);
        // setDestination's family guard leaves a foreign slot alone, and
        // the state write is a no-op off a missing slot or a wrong
        // family.
        set_navigate_step_destination(&entity_rc, MOVEMENT, [1.0, 0.0, 0.0]);
        app.write_navigation_state(
            &entity_rc,
            0x1997,
            [1.0, 0.0, 0.0],
            v3([1.0, 0.0, 0.0]),
            Vec::new(),
            0,
            None,
        );
        app.write_navigation_state(
            &entity_rc,
            MOVEMENT,
            [1.0, 0.0, 0.0],
            v3([1.0, 0.0, 0.0]),
            Vec::new(),
            0,
            None,
        );
        let borrowed = entity_rc.borrow();
        assert!(matches!(
            &borrowed.controllers.container[&MOVEMENT].kind,
            ControllerKind::MoveToPoint {
                destination: [9.0, 0.0, 0.0],
                ..
            }
        ));
        drop(borrowed);
        assert_eq!(entity_position(&app, 77), [0.0, 0.0, 0.0]);
    }

    #[test]
    fn navigate_attaches_the_full_controller_and_resolves_the_first_leg() {
        let mut app = app_with(plain_script(&["onNavigate"]));
        let mut spaces = spaces_with();
        create(&mut app, &mut spaces, 77, [0.0, 0.0, 0.0]);
        // Cross-set: the way loc is a portal waypoint in another set,
        // with a cached traversal path behind it.
        let mut nav = ScriptedNavigator::default();
        nav.resolve_answers
            .push_back(Some(loc(1, 0, [0.0, 0.0, 0.0])));
        nav.resolve_answers
            .push_back(Some(loc(2, 3, [4.0, 0.0, 0.0])));
        nav.find_path_answers
            .push_back(Some((loc(2, 3, [2.0, 0.0, 0.0]), true)));
        nav.cached_paths
            .push_back(vec![v3([2.0, 0.0, 0.0]), v3([4.0, 0.0, 0.0])]);
        nav.max_height_y = Some(0.5);
        nav.clip_to = Some(v3([4.0, 0.5, 0.0]));
        app.navigators
            .insert(77, NavigatorHandle(Rc::new(RefCell::new(nav))));

        let id = app.navigate(77, [4.0, 0.0, 0.0], 10.0, true, -1.0, 0.5, 0.01, 7);
        assert_eq!(id, Some(MOVEMENT));
        assert!(app.navigation_warnings.is_empty());

        let borrowed = app.entity_population[&77].borrow();
        // The stored destination is the clipped way point; the traversal
        // path adopts the cache and aims at its first point; the sets
        // differ, so no destination append.
        assert!(matches!(
            &borrowed.controllers.container[&MOVEMENT].kind,
            ControllerKind::Navigation {
                destination: [4.0, 0.5, 0.0],
                next_position: [2.0, 0.0, 0.0],
                path,
                current_node: 0,
                dst_loc,
                max_distance: 500.0,
                ..
            } if path == &vec![[2.0, 0.0, 0.0], [4.0, 0.0, 0.0]] && dst_loc.is_some()
        ));
        drop(borrowed);
        assert!(app.updatable_controllers.contains(&Some((77, MOVEMENT))));
    }

    #[test]
    fn navigate_flags_a_very_small_close_enough() {
        let mut app = app_with(plain_script(&[]));
        let mut spaces = spaces_with();
        create(&mut app, &mut spaces, 77, [0.0, 0.0, 0.0]);
        app.navigators.insert(
            77,
            NavigatorHandle(Rc::new(RefCell::new(ScriptedNavigator::flat(
                [1.0, 0.0, 0.0],
                8,
                None,
            )))),
        );

        // An explicit search distance passes through the
        // navigation-controller constructor untouched.
        app.navigate(77, [1.0, 0.0, 0.0], 10.0, true, 250.0, 0.5, 0.0005, 0);
        assert_eq!(app.navigation_warnings.len(), 1);
        assert!(
            app.navigation_warnings[0]
                .1
                .contains("closeEnough_ is very small")
        );
        // The constructor keeps its own -1 substitution for callers
        // that resolve later than construction.
        let kind =
            ControllerKind::navigation([1.0, 0.0, 0.0], 10.0, true, -1.0, 0.5, 0.01, 10, 500.0);
        assert!(matches!(
            kind,
            ControllerKind::Navigation { max_distance, .. } if max_distance == 500.0
        ));
    }

    #[test]
    fn navigation_without_a_navigator_hangs_inert() {
        let mut app = app_with(plain_script(&[]));
        let mut spaces = spaces_with();
        create(&mut app, &mut spaces, 77, [0.0, 0.0, 0.0]);

        let controller = Controller {
            kind: ControllerKind::Navigation {
                destination: [5.0, 0.0, 0.0],
                metres_per_tick: 1.0,
                face_movement: true,
                max_distance: 500.0,
                girth: 0.5,
                close_enough: 0.01,
                next_position: [0.0; 3],
                path: Vec::new(),
                current_node: 0,
                dst_loc: None,
            },
            user_arg: 0,
        };
        app.attach_controller(77, controller);

        assert_eq!(app.navigation_warnings.len(), 1);
        assert!(
            app.navigation_warnings[0]
                .1
                .contains("Physical Space type of entity is not supported")
        );
        // Never registered: the drive never runs.
        assert!(app.updatable_controllers.is_empty());
        app.advance_tick();
        assert_eq!(entity_position(&app, 77), [0.0, 0.0, 0.0]);
    }

    #[test]
    fn navigation_invalid_ends_fall_back_to_the_entity_position() {
        let mut app = app_with(plain_script(&["onNavigateFailed"]));
        let mut spaces = spaces_with();
        create(&mut app, &mut spaces, 77, [1.0, 2.0, 3.0]);
        // Both resolutions fail: nextPosition_ = the entity's position
        // and the destination loc stays unresolved; the controller
        // still joins the updatables.
        let mut nav = ScriptedNavigator::default();
        nav.resolve_answers.push_back(None);
        nav.resolve_answers.push_back(None);
        app.navigators
            .insert(77, NavigatorHandle(Rc::new(RefCell::new(nav))));

        app.navigate(77, [5.0, 0.0, 0.0], 10.0, true, -1.0, 0.5, 0.01, 0);
        let borrowed = app.entity_population[&77].borrow();
        assert!(matches!(
            &borrowed.controllers.container[&MOVEMENT].kind,
            ControllerKind::Navigation {
                next_position: [1.0, 2.0, 3.0],
                dst_loc: None,
                ..
            }
        ));
        drop(borrowed);
        assert!(app.updatable_controllers.contains(&Some((77, MOVEMENT))));

        // The drive: standing on its own next position is a
        // reachedNextPos, the refill cannot resolve a source, and the
        // walk fails with onNavigateFailed + cancel.
        let mut nav = ScriptedNavigator::default();
        nav.resolve_answers.push_back(None);
        app.navigators
            .insert(77, NavigatorHandle(Rc::new(RefCell::new(nav))));
        app.advance_tick();
        assert!(
            app.script_calls
                .iter()
                .any(|call| call.name == "onNavigateFailed")
        );
        assert!(
            app.entity_population[&77]
                .borrow()
                .controllers
                .container
                .is_empty()
        );

        // The twin face: the refill's fresh source resolves but the
        // stored destination loc never did, so the walk fails the same
        // way from the unresolved-destination guard.
        let mut nav = ScriptedNavigator::default();
        nav.resolve_answers
            .push_back(Some(loc(1, 0, [1.0, 2.0, 3.0])));
        nav.resolve_answers.push_back(None);
        app.navigators
            .insert(77, NavigatorHandle(Rc::new(RefCell::new(nav))));
        app.navigate(77, [5.0, 0.0, 0.0], 10.0, true, -1.0, 0.5, 0.01, 0);
        let mut nav = ScriptedNavigator::default();
        nav.resolve_answers
            .push_back(Some(loc(1, 0, [1.0, 2.0, 3.0])));
        app.navigators
            .insert(77, NavigatorHandle(Rc::new(RefCell::new(nav))));
        app.advance_tick();
        assert!(
            app.script_calls
                .iter()
                .any(|call| call.name == "onNavigateFailed")
        );
        assert!(
            app.entity_population[&77]
                .borrow()
                .controllers
                .container
                .is_empty()
        );
    }

    #[test]
    fn navigation_start_with_no_path_falls_back_to_the_position() {
        let mut app = app_with(plain_script(&[]));
        let mut spaces = spaces_with();
        create(&mut app, &mut spaces, 77, [0.0, 0.0, 0.0]);
        // Both ends resolve onto different waypoints but no path
        // exists: getNavigatePosition fails and startReal falls back to
        // the entity position (the destination loc itself resolved).
        let mut nav = ScriptedNavigator::default();
        nav.resolve_answers
            .push_back(Some(loc(1, 0, [0.0, 0.0, 0.0])));
        nav.resolve_answers
            .push_back(Some(loc(2, 3, [5.0, 0.0, 0.0])));
        nav.find_path_answers.push_back(None);
        app.navigators
            .insert(77, NavigatorHandle(Rc::new(RefCell::new(nav))));

        app.navigate(77, [5.0, 0.0, 0.0], 10.0, true, -1.0, 0.5, 0.01, 0);
        let borrowed = app.entity_population[&77].borrow();
        assert!(matches!(
            &borrowed.controllers.container[&MOVEMENT].kind,
            ControllerKind::Navigation {
                destination: [5.0, 0.0, 0.0], // the clipped point
                next_position: [0.0, 0.0, 0.0],
                path,
                dst_loc: Some(_),
                ..
            } if path.is_empty()
        ));
        drop(borrowed);
        assert!(app.updatable_controllers.contains(&Some((77, MOVEMENT))));
    }

    #[test]
    fn the_way_point_height_falls_back_to_the_source_when_unset() {
        let mut app = app_with(plain_script(&[]));
        let mut spaces = spaces_with();
        create(&mut app, &mut spaces, 77, [0.0, 0.0, 0.0]);
        // The way loc carries no single waypoint (negative): its height
        // comes from the source loc, and with an empty cached path the
        // computed leg is the next position verbatim.
        let mut nav = ScriptedNavigator::default();
        nav.resolve_answers
            .push_back(Some(loc(1, 0, [0.0, 0.0, 0.0])));
        nav.resolve_answers
            .push_back(Some(loc(2, 3, [4.0, 0.0, 0.0])));
        nav.find_path_answers
            .push_back(Some((loc(2, -1, [2.0, 0.0, 0.0]), false)));
        nav.cached_paths.push_back(Vec::new());
        nav.max_height_y = Some(1.5);
        app.navigators
            .insert(77, NavigatorHandle(Rc::new(RefCell::new(nav))));

        app.navigate(77, [4.0, 0.0, 0.0], 10.0, true, -1.0, 0.5, 0.01, 0);
        let borrowed = app.entity_population[&77].borrow();
        assert!(matches!(
            &borrowed.controllers.container[&MOVEMENT].kind,
            ControllerKind::Navigation {
                next_position: [2.0, 1.5, 0.0],
                path,
                ..
            } if path.is_empty()
        ));
    }

    #[test]
    fn navigation_walks_its_path_and_completes() {
        // The arrival callback cancels its own movement: the post-
        // callback containment check in update_navigation_controller
        // takes its false arm, and the script itself is built with
        // EntityScript::with_methods.
        let mut actions = BTreeMap::new();
        actions.insert("onNavigate", vec![MethodAction::CancelMovement]);
        let script =
            EntityScript::with_methods(BTreeSet::from(["__init__", "onNavigate"]), actions);
        let mut app = app_with(script);
        let mut spaces = spaces_with();
        create(&mut app, &mut spaces, 77, [0.0, 0.0, 0.0]);
        // Same-set resolution: the destination point is the direct leg
        // (waypointsEqual), so the walk is one leg to [3, 0, 0] with a
        // closeEnough of 0.5 and metresPerTick 1.
        app.navigators.insert(
            77,
            NavigatorHandle(Rc::new(RefCell::new(ScriptedNavigator::flat(
                [3.0, 0.0, 0.0],
                8,
                None,
            )))),
        );

        app.navigate(77, [3.0, 0.0, 0.0], 10.0, true, -1.0, 0.5, 0.5, 6);
        for _ in 0..2 {
            app.advance_tick();
        }
        // Tick 1 walks to x=1; tick 2 to x=2 — not yet complete.
        assert_eq!(entity_position(&app, 77)[0], 2.0);
        assert_eq!(app.script_calls.len(), 1); // only the creation __init__
        app.advance_tick(); // arrival: |3-3| < 0.5 fires onNavigate + cancel
        let last = app.script_calls.last().expect("onNavigate fired");
        assert_eq!(last.name, "onNavigate");
        assert_eq!(last.timer, Some((MOVEMENT, 6)));
        assert_eq!(entity_position(&app, 77), [3.0, 0.0, 0.0]);
        assert!(
            app.entity_population[&77]
                .borrow()
                .controllers
                .container
                .is_empty()
        );
    }

    #[test]
    fn navigation_advances_then_refills_its_traversal_path() {
        let mut app = app_with(plain_script(&["onNavigate"]));
        let mut spaces = spaces_with();
        create(&mut app, &mut spaces, 77, [0.0, 0.0, 0.0]);
        // A cross-set first leg whose cached traversal path holds only
        // the portal point; exhausting it drives the refill.
        let clear_counter = Rc::new(RefCell::new(0u32));
        let mut nav = ScriptedNavigator {
            clear_way_path_counter: clear_counter.clone(),
            ..ScriptedNavigator::default()
        };
        nav.resolve_answers
            .push_back(Some(loc(1, 0, [0.0, 0.0, 0.0])));
        nav.resolve_answers
            .push_back(Some(loc(2, 3, [3.0, 0.0, 0.0])));
        nav.find_path_answers
            .push_back(Some((loc(2, 4, [1.0, 0.0, 0.0]), false)));
        nav.cached_paths.push_back(vec![v3([1.0, 0.0, 0.0])]);
        // The refill's cached traversal path, same set as the
        // destination now.
        nav.cached_paths.push_back(vec![v3([3.0, 0.0, 0.0])]);
        app.navigators
            .insert(77, NavigatorHandle(Rc::new(RefCell::new(nav))));

        app.navigate(77, [3.0, 0.0, 0.0], 10.0, true, -1.0, 0.5, 0.01, 0);

        // tick1 walks to the portal point and its arrival advances the
        // path cursor; tick2's arrival on the same point finds the path
        // exhausted and refills (the re-resolved source now matches the
        // destination's waypoint, so the refill aims straight at
        // [3,0,0] with the destination appended).
        app.advance_tick();
        assert_eq!(entity_position(&app, 77)[0], 1.0);
        app.advance_tick();
        assert_eq!(*clear_counter.borrow(), 2); // startReal + refill
        // ticks 3-5 walk the refilled leg (2 → 3 → arrive) and tick 5
        // lands on the destination and completes.
        for _ in 0..3 {
            app.advance_tick();
        }
        assert!(
            app.script_calls
                .iter()
                .any(|call| call.name == "onNavigate")
        );
        assert_eq!(entity_position(&app, 77), [3.0, 0.0, 0.0]);
        assert!(
            app.entity_population[&77]
                .borrow()
                .controllers
                .container
                .is_empty()
        );

        // A find_path-less refill is the failure face: the walk ends
        // with onNavigateFailed and the controller cancels itself.
        let mut app2 = app_with(plain_script(&["onNavigateFailed"]));
        let mut spaces2 = spaces_with();
        create(&mut app2, &mut spaces2, 78, [0.0, 0.0, 0.0]);
        let mut nav = ScriptedNavigator::default();
        nav.resolve_answers
            .push_back(Some(loc(1, 0, [0.0, 0.0, 0.0])));
        nav.resolve_answers
            .push_back(Some(loc(2, 3, [5.0, 0.0, 0.0])));
        // The refill's source resolution at [1,0,0] lands back in the
        // origin set — unlike the destination's, so the search runs.
        nav.resolve_answers
            .push_back(Some(loc(1, 0, [1.0, 0.0, 0.0])));
        nav.find_path_answers
            .push_back(Some((loc(2, 4, [1.0, 0.0, 0.0]), false)));
        nav.cached_paths.push_back(Vec::new()); // empty traversal path
        nav.find_path_answers.push_back(None); // the refill's search fails
        app2.navigators
            .insert(78, NavigatorHandle(Rc::new(RefCell::new(nav))));
        app2.navigate(78, [5.0, 0.0, 0.0], 10.0, true, -1.0, 0.5, 0.01, 0);
        // nextPosition is the way point (the empty path leaves it);
        // arrival on it exhausts the (empty) path and the refill's
        // search fails → onNavigateFailed.
        app2.advance_tick();
        assert!(
            app2.script_calls
                .iter()
                .any(|call| call.name == "onNavigateFailed")
        );
        assert!(
            app2.entity_population[&78]
                .borrow()
                .controllers
                .container
                .is_empty()
        );
    }

    #[test]
    fn navigation_clamps_its_height_when_the_next_position_is_above() {
        let mut app = app_with(plain_script(&["onNavigate"]));
        let mut spaces = spaces_with();
        create(&mut app, &mut spaces, 77, [0.0, 0.0, 0.0]);
        // The walk leg rises: walking 1 of the 5.831 metres toward
        // [5, 3, 0] clamps the y up to the next position's 3 (the
        // climb-only rule).
        app.navigators.insert(
            77,
            NavigatorHandle(Rc::new(RefCell::new(ScriptedNavigator::flat(
                [5.0, 3.0, 0.0],
                8,
                None,
            )))),
        );
        app.navigate(77, [5.0, 3.0, 0.0], 10.0, true, -1.0, 0.5, 0.01, 0);
        app.advance_tick();
        let position = entity_position(&app, 77);
        assert!((position[0] - 0.857493).abs() < 1e-4); // 1 metre along the 5.831-length leg
        assert_eq!(position[1], 3.0); // clamped up
        // Facing: the yaw follows the movement (atan2(x, z) = π/2).
        let direction = app.entity_population[&77].borrow().direction;
        assert!((direction[0] - std::f32::consts::FRAC_PI_2).abs() < 1e-5);
    }

    #[test]
    fn navigation_zeroes_a_descending_step_before_the_clamp() {
        let mut app = app_with(plain_script(&[]));
        let mut spaces = spaces_with();
        create(&mut app, &mut spaces, 77, [0.0, 0.0, 0.0]);
        // The next position sits below the entity: the descent is
        // zeroed before the walk, so the leg stays flat and the
        // climb-only clamp never pulls the y down.
        app.navigators.insert(
            77,
            NavigatorHandle(Rc::new(RefCell::new(ScriptedNavigator::flat(
                [5.0, -2.0, 0.0],
                8,
                None,
            )))),
        );
        app.navigate(77, [5.0, -2.0, 0.0], 10.0, true, -1.0, 0.5, 0.01, 0);
        app.advance_tick();
        assert_eq!(entity_position(&app, 77), [1.0, 0.0, 0.0]);
    }

    #[test]
    fn a_lost_navigator_leaves_the_walk_coasting() {
        let mut app = app_with(plain_script(&[]));
        let mut spaces = spaces_with();
        create(&mut app, &mut spaces, 77, [0.0, 0.0, 0.0]);
        // Cross-set first leg with an empty cached path: the walk aims
        // at the way point [2,0,0] while the destination is [5,0,0].
        let mut nav = ScriptedNavigator::default();
        nav.resolve_answers
            .push_back(Some(loc(1, 0, [0.0, 0.0, 0.0])));
        nav.resolve_answers
            .push_back(Some(loc(2, 3, [5.0, 0.0, 0.0])));
        nav.find_path_answers
            .push_back(Some((loc(2, 4, [2.0, 0.0, 0.0]), false)));
        nav.cached_paths.push_back(Vec::new());
        app.navigators
            .insert(77, NavigatorHandle(Rc::new(RefCell::new(nav))));
        app.navigate(77, [5.0, 0.0, 0.0], 10.0, true, -1.0, 0.5, 0.01, 0);
        // White-box: the extra's nav system dies (the reference cannot
        // do this without the entity dying too).
        app.navigators.remove(&77);
        app.advance_tick(); // walks 1 of the 2 metres to the way point
        app.advance_tick(); // arrives: reachedNextPos with an exhausted path → coast
        assert_eq!(entity_position(&app, 77), [2.0, 0.0, 0.0]);
        // No callback fired: the walk just coasts (only the creation
        // __init__ is on the ledger).
        assert_eq!(app.script_calls.len(), 1);
        assert_eq!(app.script_calls[0].name, "__init__");
    }

    #[test]
    fn the_navigation_drive_tolerates_a_missing_slot() {
        let mut app = app_with(plain_script(&[]));
        let mut spaces = spaces_with();
        create(&mut app, &mut spaces, 77, [0.0, 0.0, 0.0]);
        app.navigators.insert(
            77,
            NavigatorHandle(Rc::new(RefCell::new(ScriptedNavigator::flat(
                [5.0, 0.0, 0.0],
                8,
                None,
            )))),
        );
        app.navigate(77, [5.0, 0.0, 0.0], 10.0, true, -1.0, 0.5, 0.01, 0);
        let entity_rc = app.entity_population[&77].clone();

        // White-box: a missing controller leaves the walk where it is.
        assert_eq!(
            app.navigation_move(&entity_rc, 0x1997),
            NavigationStatus::InProgress
        );
        assert_eq!(entity_position(&app, 77), [0.0, 0.0, 0.0]);
        // A family swap under the id answers the same way.
        entity_rc.borrow_mut().controllers.container.insert(
            MOVEMENT,
            Controller {
                kind: ControllerKind::MoveToPoint {
                    destination: [9.0, 0.0, 0.0],
                    metres_per_tick: 1.0,
                    face_movement: false,
                    move_vertically: false,
                },
                user_arg: 0,
            },
        );
        assert_eq!(
            app.navigation_move(&entity_rc, MOVEMENT),
            NavigationStatus::InProgress
        );
        assert_eq!(entity_position(&app, 77), [0.0, 0.0, 0.0]);
    }

    #[test]
    fn can_navigate_to_answers_the_destination_point() {
        let mut app = app_with(plain_script(&[]));
        let mut spaces = spaces_with();
        create(&mut app, &mut spaces, 77, [0.0, 0.0, 0.0]);
        // Same waypoint: the direct answer, raised onto the surface.
        let mut nav = ScriptedNavigator::default();
        nav.resolve_answers
            .push_back(Some(loc(1, 0, [0.0, 0.0, 0.0])));
        nav.resolve_answers
            .push_back(Some(loc(1, 0, [2.0, 0.0, 0.0])));
        nav.max_height_y = Some(7.0);
        app.navigators
            .insert(77, NavigatorHandle(Rc::new(RefCell::new(nav))));
        assert_eq!(
            app.can_navigate_to(77, [2.0, 0.0, 0.0], 100.0, 0.5),
            Some([2.0, 7.0, 0.0])
        );

        // Cross-set: the path search answers, and the destination's own
        // surface sets the height.
        let mut app2 = app_with(plain_script(&[]));
        let mut spaces2 = spaces_with();
        create(&mut app2, &mut spaces2, 77, [0.0, 0.0, 0.0]);
        let mut nav = ScriptedNavigator::default();
        nav.resolve_answers
            .push_back(Some(loc(1, 0, [0.0, 0.0, 0.0])));
        nav.resolve_answers
            .push_back(Some(loc(2, 1, [4.0, 0.0, 0.0])));
        nav.find_path_answers
            .push_back(Some((loc(2, 1, [4.0, 0.0, 0.0]), false)));
        nav.max_height_y = Some(3.0);
        app2.navigators
            .insert(77, NavigatorHandle(Rc::new(RefCell::new(nav))));
        assert_eq!(
            app2.can_navigate_to(77, [4.0, 0.0, 0.0], -1.0, 0.5),
            Some([4.0, 3.0, 0.0])
        );

        // No path: None (the Py_RETURN_NONE face).
        let mut app3 = app_with(plain_script(&[]));
        let mut spaces3 = spaces_with();
        create(&mut app3, &mut spaces3, 77, [0.0, 0.0, 0.0]);
        let mut nav = ScriptedNavigator::default();
        nav.resolve_answers
            .push_back(Some(loc(1, 0, [0.0, 0.0, 0.0])));
        nav.resolve_answers
            .push_back(Some(loc(2, 1, [4.0, 0.0, 0.0])));
        nav.find_path_answers.push_back(None);
        app3.navigators
            .insert(77, NavigatorHandle(Rc::new(RefCell::new(nav))));
        assert_eq!(app3.can_navigate_to(77, [4.0, 0.0, 0.0], -1.0, 0.5), None);

        // An unresolvable source stands off the mesh entirely: the
        // ends guard answers None without a path search.
        let mut app4 = app_with(plain_script(&[]));
        let mut spaces4 = spaces_with();
        create(&mut app4, &mut spaces4, 77, [0.0, 0.0, 0.0]);
        app4.navigators.insert(
            77,
            NavigatorHandle(Rc::new(RefCell::new(ScriptedNavigator::default()))),
        );
        assert_eq!(app4.can_navigate_to(77, [1.0, 0.0, 0.0], -1.0, 0.5), None);
    }

    #[test]
    fn can_navigate_to_refuses_a_ghost() {
        let mut app = app_with(plain_script(&[]));
        let mut spaces = spaces_with();
        create(&mut app, &mut spaces, 77, [0.0, 0.0, 0.0]);
        // An absent id is not a real entity either.
        assert_eq!(app.can_navigate_to(999, [1.0, 0.0, 0.0], -1.0, 0.5), None);
        assert_eq!(
            app.script_errors[0].1,
            "This method can only be called on a real entity."
        );
        // A ghost (is_real false) draws the same TypeError.
        app.entity_population.get(&77).unwrap().borrow_mut().is_real = false;
        assert_eq!(app.can_navigate_to(77, [1.0, 0.0, 0.0], -1.0, 0.5), None);
        assert_eq!(
            app.script_errors[1].1,
            "This method can only be called on a real entity."
        );
        // The full controller refusal is the same face one layer up.
        assert_eq!(
            app.navigate(77, [1.0, 0.0, 0.0], 10.0, true, -1.0, 0.5, 0.01, 0),
            None
        );
        assert_eq!(
            app.script_errors[2].1,
            "This method can only be called on a real entity."
        );
    }

    #[test]
    fn destroying_the_entity_releases_the_navigate_state() {
        let mut app = app_with(plain_script(&[]));
        let mut spaces = spaces_with();
        create(&mut app, &mut spaces, 77, [0.0, 0.0, 0.0]);
        app.navigators.insert(
            77,
            NavigatorHandle(Rc::new(RefCell::new(ScriptedNavigator::flat(
                [1.0, 0.0, 0.0],
                4,
                None,
            )))),
        );
        app.navigate_step(77, [1.0, 0.0, 0.0], 10.0, 100.0, -1.0, true, 0.5, 0);
        assert!(app.navigate_step_controllers.contains_key(&77));

        app.destroy_entity(77);
        assert!(!app.navigate_step_controllers.contains_key(&77));
        assert!(!app.navigators.contains_key(&77));
    }

    #[test]
    fn real_controllers_round_trip_through_the_offload_stream() {
        let mut app = app_with(plain_script(&["onMove", "onNavigate", "onTimer"]));
        let mut spaces = spaces_with();
        create(&mut app, &mut spaces, 77, [0.0, 0.0, 0.0]);
        // The navigate faces need a waypoint stack on the entity; the
        // flat answer puts every hop at [1,0,0].
        app.navigators.insert(
            77,
            NavigatorHandle(Rc::new(RefCell::new(ScriptedNavigator::flat(
                [1.0, 0.0, 0.0],
                8,
                None,
            )))),
        );
        app.add_timer(77, 1.0, 0.0, 11);
        // A movement family under the shared exclusive id.
        app.attach_controller(
            77,
            Controller {
                kind: ControllerKind::move_to_entity(78, 1.5, 5.0, true, false, 10),
                user_arg: 5,
            },
        );
        let data_move_entity = app.write_real_controllers_to_stream(77).unwrap();
        // 8-byte head + timer record (id/type/userArg + 8-byte payload)
        // + the move-to-entity record (4+1+1+4+4 payload).
        assert_eq!(data_move_entity.len(), 8 + 20 + 26);

        app.cancel_controller(77, MOVEMENT);
        app.navigate_step(77, [1.0, 0.0, 0.0], 10.0, 100.0, -1.0, true, 0.5, 9);
        let data_step = app.write_real_controllers_to_stream(77).unwrap();
        // The navigate-step payload ends with its destination.
        let tail = &data_step[data_step.len() - 12..];
        assert_eq!(tail[0..4], 1.0f32.to_le_bytes());
        assert_eq!(tail[4..8], 0.0f32.to_le_bytes());
        assert_eq!(tail[8..12], 0.0f32.to_le_bytes());

        app.cancel_controller(77, MOVEMENT);
        app.navigate(77, [3.0, 0.0, 0.0], 10.0, true, -1.0, 0.5, 0.01, 7);
        let data_full = app.write_real_controllers_to_stream(77).unwrap();

        // The other three movement families share the same exclusive
        // id (they replace each other), so each streams its own block.
        app.cancel_controller(77, MOVEMENT);
        app.attach_controller(
            77,
            Controller {
                kind: ControllerKind::yaw_rotator(1.2, 6.0, 10),
                user_arg: 0,
            },
        );
        let data_yaw = app.write_real_controllers_to_stream(77).unwrap();
        app.cancel_controller(77, MOVEMENT);
        app.attach_controller(
            77,
            Controller {
                kind: ControllerKind::accelerate_to_point(
                    [6.0, 2.0, 0.0],
                    1.0,
                    4.0,
                    AccelerationFacing::Velocity,
                    true,
                ),
                user_arg: 0,
            },
        );
        let data_accelerate = app.write_real_controllers_to_stream(77).unwrap();
        app.cancel_controller(77, MOVEMENT);
        app.attach_controller(
            77,
            Controller {
                kind: ControllerKind::move_to_point([8.0, 0.0, 0.0], 5.0, true, false, 10),
                user_arg: 0,
            },
        );
        let data_move = app.write_real_controllers_to_stream(77).unwrap();

        // The remaining two facings round-trip through their own codes.
        app.cancel_controller(77, MOVEMENT);
        app.attach_controller(
            77,
            Controller {
                kind: ControllerKind::accelerate_to_point(
                    [2.0, 0.0, 0.0],
                    0.5,
                    3.0,
                    AccelerationFacing::None,
                    false,
                ),
                user_arg: 0,
            },
        );
        let data_no_facing = app.write_real_controllers_to_stream(77).unwrap();
        app.cancel_controller(77, MOVEMENT);
        app.attach_controller(
            77,
            Controller {
                kind: ControllerKind::accelerate_to_point(
                    [2.0, 0.0, 0.0],
                    0.5,
                    3.0,
                    AccelerationFacing::Acceleration,
                    false,
                ),
                user_arg: 0,
            },
        );
        let data_full_facing = app.write_real_controllers_to_stream(77).unwrap();

        // A fresh app restores each block: the timer clamps its stale
        // start, the navigate-step re-arms its pointer, the full
        // controller re-resolves its first leg.
        type RestoreCheck = fn(&CellApp);
        let expectations: [(&Vec<u8>, RestoreCheck); 8] = [
            (&data_move_entity, |restored: &CellApp| {
                let borrowed = restored.entity_population[&77].borrow();
                assert!(matches!(
                    borrowed.controllers.container[&MOVEMENT].kind,
                    ControllerKind::MoveToEntity {
                        resolved: false,
                        ..
                    }
                ));
                assert!(
                    restored
                        .updatable_controllers
                        .contains(&Some((77, MOVEMENT)))
                );
            }),
            (&data_step, |restored: &CellApp| {
                assert_eq!(restored.navigate_step_controllers.get(&77), Some(&MOVEMENT));
                let borrowed = restored.entity_population[&77].borrow();
                assert!(matches!(
                    &borrowed.controllers.container[&MOVEMENT].kind,
                    ControllerKind::NavigateStep {
                        destination: [1.0, 0.0, 0.0],
                        ..
                    }
                ));
                // The timer came along and restarted at the clamp.
                assert!(matches!(
                    &borrowed.controllers.container[&1].kind,
                    ControllerKind::Timer { start, .. } if *start == 50
                ));
            }),
            (&data_full, |restored: &CellApp| {
                let borrowed = restored.entity_population[&77].borrow();
                // startReal re-resolved against the flat navigator:
                // the direct leg with a live destination loc.
                assert!(matches!(
                    &borrowed.controllers.container[&MOVEMENT].kind,
                    ControllerKind::Navigation {
                        next_position: [3.0, 0.0, 0.0],
                        dst_loc: Some(_),
                        ..
                    }
                ));
                assert!(
                    restored
                        .updatable_controllers
                        .contains(&Some((77, MOVEMENT)))
                );
            }),
            (
                &data_yaw,
                // The read side keeps the normalised yaw and the
                // per-tick division the constructor applied.
                |restored: &CellApp| {
                    assert_restored_kind(restored, 2, |kind| {
                        matches!(
                            kind,
                            ControllerKind::YawRotator {
                                target_yaw,
                                radians_per_tick,
                            } if *target_yaw == 1.2
                                && (*radians_per_tick - 0.6).abs() < 1e-6
                        )
                    });
                },
            ),
            (&data_accelerate, |restored: &CellApp| {
                assert_restored_kind(restored, 2, |kind| {
                    matches!(
                        kind,
                        ControllerKind::AccelerateToPoint {
                            destination: [6.0, 2.0, 0.0],
                            acceleration: 1.0,
                            max_speed: 4.0,
                            facing: AccelerationFacing::Velocity,
                            stop_at_destination: true,
                        }
                    )
                });
            }),
            (&data_move, |restored: &CellApp| {
                assert_restored_kind(restored, 2, |kind| {
                    matches!(
                        kind,
                        ControllerKind::MoveToPoint {
                            destination: [8.0, 0.0, 0.0],
                            metres_per_tick,
                            face_movement: true,
                            move_vertically: false,
                        } if (metres_per_tick - 0.5).abs() < 1e-6
                    )
                });
            }),
            (&data_no_facing, |restored: &CellApp| {
                assert_restored_kind(restored, 2, |kind| {
                    matches!(
                        kind,
                        ControllerKind::AccelerateToPoint {
                            facing: AccelerationFacing::None,
                            ..
                        }
                    )
                });
            }),
            (&data_full_facing, |restored: &CellApp| {
                assert_restored_kind(restored, 2, |kind| {
                    matches!(
                        kind,
                        ControllerKind::AccelerateToPoint {
                            facing: AccelerationFacing::Acceleration,
                            ..
                        }
                    )
                });
            }),
        ];
        for (data, check) in expectations {
            let mut restored = app_with(plain_script(&["onMove", "onNavigate", "onTimer"]));
            let mut spaces2 = spaces_with();
            restored.time = 50;
            create(&mut restored, &mut spaces2, 77, [0.0, 0.0, 0.0]);
            restored.navigators.insert(
                77,
                NavigatorHandle(Rc::new(RefCell::new(ScriptedNavigator::flat(
                    [3.0, 0.0, 0.0],
                    8,
                    None,
                )))),
            );
            assert!(restored.read_real_controllers_from_stream(77, data));
            let entity_rc = restored.entity_population[&77].clone();
            crate::cellapp_real_entity::start_real_controllers(&mut restored, &entity_rc);
            check(&restored);
        }
    }

    #[test]
    fn a_malformed_offload_block_is_refused() {
        let mut app = app_with(plain_script(&[]));
        let mut spaces = spaces_with();
        create(&mut app, &mut spaces, 77, [0.0, 0.0, 0.0]);
        // Truncated: no lastAllocatedID at all.
        assert!(!app.read_real_controllers_from_stream(77, &[]));
        // Every later truncation draws its own refusal: the count, the
        // controller id, the type code and the user arg.
        let mut stream = BinaryStream::new();
        stream.write_u32(1);
        assert!(!app.read_real_controllers_from_stream(77, stream.as_bytes()));
        let mut stream = BinaryStream::new();
        stream.write_u32(1).write_u32(1);
        assert!(!app.read_real_controllers_from_stream(77, stream.as_bytes()));
        let mut stream = BinaryStream::new();
        stream.write_u32(1).write_u32(1).write_u32(5);
        assert!(!app.read_real_controllers_from_stream(77, stream.as_bytes()));
        let mut stream = BinaryStream::new();
        stream
            .write_u32(1)
            .write_u32(1)
            .write_u32(5)
            .write_u32(CONTROLLER_TYPE_TIMER);
        assert!(!app.read_real_controllers_from_stream(77, stream.as_bytes()));
        // Unknown type code.
        let mut stream = BinaryStream::new();
        stream
            .write_u32(1)
            .write_u32(1)
            .write_u32(5)
            .write_u32(99)
            .write_i32(0);
        assert!(!app.read_real_controllers_from_stream(77, stream.as_bytes()));
        // Out-of-range facing on the acceleration payload.
        let mut stream = BinaryStream::new();
        stream
            .write_u32(1)
            .write_u32(1)
            .write_u32(9)
            .write_u32(CONTROLLER_TYPE_ACCELERATE_TO_POINT)
            .write_i32(0);
        stream
            .write_f32(1.0)
            .write_f32(2.0)
            .write_i32(7)
            .write_f32(0.0)
            .write_f32(0.0)
            .write_f32(0.0);
        assert!(!app.read_real_controllers_from_stream(77, stream.as_bytes()));
        // An absent entity answers false too.
        assert!(!app.read_real_controllers_from_stream(999, &[0; 8]));
    }

    #[test]
    fn restore_real_entity_answers_on_restore_and_restarts_the_drives() {
        // The offloading side: an entity mid-walk under a navigate-step
        // controller plus a timer.
        let mut app = app_with(plain_script(&["onRestore", "onMove", "onTimer"]));
        let mut spaces = spaces_with();
        create(&mut app, &mut spaces, 77, [0.0, 0.0, 0.0]);
        app.navigators.insert(
            77,
            NavigatorHandle(Rc::new(RefCell::new(ScriptedNavigator::flat(
                [0.5, 0.0, 0.0],
                8,
                None,
            )))),
        );
        app.navigate_step(77, [0.5, 0.0, 0.0], 10.0, 100.0, -1.0, true, 0.5, 9);
        app.add_timer(77, 1.0, 0.0, 11);
        let controllers = app.write_real_controllers_to_stream(77).unwrap();
        let mut stream = create_stream(77, TYPE, [0.0, 0.0, 0.0]);
        stream.extend_from_slice(&controllers);

        // The restoring side: a fresh app at a later time.
        let mut restored = app_with(plain_script(&["onRestore", "onMove", "onTimer"]));
        restored.time = 40;
        let mut spaces2 = spaces_with();
        restored.navigators.insert(
            77,
            NavigatorHandle(Rc::new(RefCell::new(ScriptedNavigator::flat(
                [0.5, 0.0, 0.0],
                8,
                None,
            )))),
        );
        let outcome = restored.restore_real_entity(&mut spaces2, SID, &stream);
        assert!(matches!(outcome, CreateRealOutcome::Created(_)));

        // onRestore answered (not __init__), and the controllers are
        // re-armed: the navigate-step pointer and the clamped timer.
        assert_eq!(restored.script_calls.len(), 1);
        assert_eq!(restored.script_calls[0].name, "onRestore");
        assert_eq!(restored.navigate_step_controllers.get(&77), Some(&MOVEMENT));
        let borrowed = restored.entity_population[&77].borrow();
        assert!(matches!(
            &borrowed.controllers.container[&1].kind,
            ControllerKind::Timer { start, .. } if *start == 40
        ));
        drop(borrowed);

        // The baseline continues: one tick completes the hop and fires
        // onMove.
        restored.advance_tick();
        assert!(
            restored
                .script_calls
                .iter()
                .any(|call| call.name == "onMove")
        );
        assert_eq!(
            restored.entity_population[&77].borrow().position,
            [0.5, 0.0, 0.0]
        );
    }

    #[test]
    fn the_offload_stream_is_id_ordered_and_stable() {
        let mut app = app_with(plain_script(&[]));
        let mut spaces = spaces_with();
        create(&mut app, &mut spaces, 77, [0.0, 0.0, 0.0]);
        app.add_timer(77, 1.0, 0.0, 11);
        let first = app.write_real_controllers_to_stream(77).unwrap();
        let second = app.write_real_controllers_to_stream(77).unwrap();
        assert_eq!(first, second);
        // lastAllocatedID (1) then the count (1), then the timer's id,
        // type and userArg.
        assert_eq!(&first[0..4], &1u32.to_le_bytes());
        assert_eq!(&first[4..8], &1u32.to_le_bytes());
        assert_eq!(&first[8..12], &1u32.to_le_bytes());
        assert_eq!(&first[12..16], &CONTROLLER_TYPE_TIMER.to_le_bytes());
        assert_eq!(&first[16..20], &11i32.to_le_bytes());
    }

    #[test]
    fn an_exclusive_placeholder_streams_its_own_code() {
        let mut app = app_with(plain_script(&[]));
        let mut spaces = spaces_with();
        create(&mut app, &mut spaces, 77, [0.0, 0.0, 0.0]);
        let navigation = app.get_exclusive_id("Navigation");
        app.attach_controller(
            77,
            Controller {
                kind: ControllerKind::Exclusive {
                    category: navigation,
                },
                user_arg: 0,
            },
        );
        let data = app.write_real_controllers_to_stream(77).unwrap();
        // The placeholder round-trips with its category id.
        let mut restored = app_with(plain_script(&[]));
        let mut spaces2 = spaces_with();
        create(&mut restored, &mut spaces2, 77, [0.0, 0.0, 0.0]);
        assert!(restored.read_real_controllers_from_stream(77, &data));
        let borrowed = restored.entity_population[&77].borrow();
        assert!(matches!(
            borrowed.controllers.container[&navigation].kind,
            ControllerKind::Exclusive { .. }
        ));
    }
}
