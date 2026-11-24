//! The navigation controller (`server/cellapp/navigation_controller.*`):
//! the exclusive `"Movement"` controller that walks an entity across the
//! navigation mesh to a destination, one `metresPerTick_` hop per tick,
//! re-querying the navigator whenever the current hop completes.
//!
//! The mesh itself is the waypoint lane's deliverable — the controller
//! reaches it through the [`Navigator`] boundary, the
//! `NavmeshEntityNavigationSystem` faces it touches: navloc creation with
//! `clip()`, `NavLoc::pSet()`, `getNavigatePosition`, and the cached
//! waypoint path.
//!
//! The BW-08-02-04 acceptance is "退出或迁移后状态满足基线": the state
//! streams as `destination/metresPerTick/faceMovement/maxDistance/girth`
//! (`navigation_controller.cpp:250-281`), the restore re-seeds
//! `nextPosition_` from the destination, and the restart re-resolves the
//! mesh — or refuses outright in a non-chunk (delegate) space, the same
//! `!entity().pChunkSpace()` warning face as the reference.

use foundation::math::Vector3;
use foundation::stream::{BinaryStream, StreamError};

use crate::controllers::ControllerId;
use crate::movement::{EntityState, ScriptCallback};

/// `NavigationStatus` (`navigation_controller.hpp:31-36`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NavigationStatus {
    /// `NAVIGATION_CANCELLED`: a script callback cancelled the controller
    /// mid-move. Unreachable in the port (callbacks are recorded, not
    /// re-entered), kept for status fidelity.
    Cancelled = -2,
    /// `NAVIGATION_FAILED`: the mesh could not resolve the next hop.
    Failed = -1,
    /// `NAVIGATION_IN_PROGRESS`.
    InProgress = 0,
    /// `NAVIGATION_COMPLETE`.
    Complete = 1,
}

/// The navigator boundary — the `NavmeshEntityNavigationSystem` faces the
/// controller touches (`navigator().createPathNavLoc(...)`,
/// `nens.getNavigatePosition(...)`, `getCachedWaypointPath`). The
/// waypoint lane implements this over the real mesh.
pub trait Navigator {
    /// `NavLoc( pChunkSpace(), point, girth )` + `clip()`: the mesh point
    /// for `point`, or `None` when the navloc is invalid. The validity
    /// face of an unclipped `NavLoc` folds into the same query.
    fn clip_to_mesh(&mut self, point: Vector3, girth: f32) -> Option<Vector3>;

    /// `nens.getNavigatePosition( srcLoc, *pDstLoc_, maxDistance_, ... )`:
    /// the next walkable point towards `destination`, or `None` when no
    /// route exists within `max_distance`.
    fn navigate_position(
        &mut self,
        source: Vector3,
        destination: Vector3,
        max_distance: f32,
        girth: f32,
    ) -> Option<Vector3>;

    /// `NavLoc::pSet()`: the waypoint set containing `point` (`None` =
    /// invalid navloc). `generateTraversalPath` appends the exact
    /// destination when source and destination share a set.
    fn waypoint_set(&mut self, point: Vector3, girth: f32) -> Option<u64>;

    /// `navigator().getCachedWaypointPath( path_ )`: the traversal path
    /// of the last query.
    fn cached_waypoint_path(&mut self) -> Vec<Vector3>;

    /// `clearCachedWayPath()` + `clearCachedWaySetPath()`.
    fn clear_cached_paths(&mut self);
}

/// `isZero` (`stdmf_minimal.hpp:113-141`) — an exact zero test.
fn is_zero(value: f32) -> bool {
    value == 0.0
}

/// The `NavigationController`. `dst_navloc` folds `pDstLoc_` into the
/// clipped destination point — `None` is the invalid-navloc face
/// (the reference's navloc object exists but `valid()` is false).
#[derive(Clone, Debug)]
pub struct NavigationController {
    /// `controllerID_`.
    pub id: ControllerId,
    /// `userArg_`.
    pub user_arg: i32,
    /// `metresPerTick_ = velocity / updateHertz`
    /// (`navigation_controller.cpp:58`).
    pub metres_per_tick: f32,
    /// `maxDistance_` — `-1` resolved to `CellAppConfig::maxAoIRadius()`.
    pub max_distance: f32,
    /// `girth_`.
    pub girth: f32,
    /// `closeEnough_`.
    pub close_enough: f32,
    /// `faceMovement_`.
    pub face_movement: bool,
    /// `nextPosition_`: the current mesh hop.
    pub next_position: Vector3,
    /// `destination_` — snapped to the mesh at start.
    pub destination: Vector3,
    /// `pDstLoc_->point()`: the clipped destination, `None` while the
    /// destination navloc is invalid.
    dst_navloc: Option<Vector3>,
    /// `path_`: the traversal path of the current query.
    path: Vec<Vector3>,
    /// `currentNode_`.
    current_node: usize,
    /// `CellApp::registerForUpdate( this )` — false also covers the
    /// non-chunk-space start that returns before registering.
    pub registered: bool,
    /// The `WARNING_MSG` log.
    pub warnings: Vec<&'static str>,
    /// `standardCallback` records, in firing order.
    pub events: Vec<ScriptCallback>,
    /// `Controllers::delController` ran.
    cancelled: bool,
}

impl NavigationController {
    /// The constructor (`navigation_controller.cpp:51-87`). `isEqual(
    /// maxDistance_, -1.f )` is an exact test; `max_aoi_radius` is
    /// `CellAppConfig::maxAoIRadius()`. A `closeEnough` below 0.001
    /// records the reference's warning.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id: ControllerId,
        user_arg: i32,
        destination: Vector3,
        velocity: f32,
        face_movement: bool,
        max_distance: f32,
        girth: f32,
        close_enough: f32,
        hertz: f32,
        max_aoi_radius: f32,
    ) -> Self {
        let mut max_distance = max_distance;
        if max_distance == -1.0 {
            max_distance = max_aoi_radius;
        }

        let mut warnings = Vec::new();
        if close_enough < 0.001 {
            // WARNING_MSG( "NavigationController::NavigationController: "
            //     "closeEnough_ is very small (%f)" )
            warnings.push("NavigationController::NavigationController: closeEnough_ is very small");
        }

        Self {
            id,
            user_arg,
            metres_per_tick: velocity / hertz,
            max_distance,
            girth,
            close_enough,
            face_movement,
            next_position: Vector3::ZERO,
            destination,
            dst_navloc: None,
            path: Vec::new(),
            current_node: 0,
            registered: false,
            warnings,
            events: Vec::new(),
            cancelled: false,
        }
    }

    /// `NavigationController::move` (`navigation_controller.cpp:72-190`):
    /// advance the entity one `metresPerTick_` hop towards
    /// `nextPosition_`, never walking down, then re-query the navigator
    /// whenever a hop completes. The reference's `MF_ASSERT(
    /// !this->entity().pVehicle() )` belongs to BW-08-02-05's vehicle
    /// faces.
    pub fn r#move(
        &mut self,
        state: &mut EntityState,
        navigator: &mut dyn Navigator,
        now: u32,
        hertz: f32,
    ) -> NavigationStatus {
        let mut position = state.position;
        let mut direction = state.direction;

        let mut movement = self.next_position - position;
        if movement.y < 0.0 {
            movement.y = 0.0;
        }

        // If we are real close, then finish up.
        if movement.length() < self.metres_per_tick {
            position = self.next_position;
        } else {
            movement.normalise();
            movement = movement * self.metres_per_tick;
            position = position + movement;
            if position.y < self.next_position.y {
                position.y = self.next_position.y;
            }
        }

        // Make sure we are facing the right direction.
        if self.face_movement && (!is_zero(movement.x) || !is_zero(movement.z)) {
            direction.yaw = movement.yaw();
        }

        // No longer on the ground...
        state.is_on_ground = false;
        // (`setAndDropGlobalPositionAndDirection`; the drop face is the
        // ghost update. Its script callbacks can cancel the controller in
        // the reference — the `!isAttached()` NAVIGATION_CANCELLED return
        // — but the port records callbacks instead, so nothing re-enters
        // here.)
        state.set_position_and_direction(position, direction, now, hertz);

        // Check for completion.
        let reached_final_pos = (position.x - self.destination.x).abs() < self.close_enough
            && (position.z - self.destination.z).abs() < self.close_enough;
        if reached_final_pos {
            return NavigationStatus::Complete;
        }

        // .. otherwise check if we are at the next update location.
        let reached_next_pos = (position.x - self.next_position.x).abs() < 0.001
            && (position.z - self.next_position.z).abs() < 0.001;
        if reached_next_pos {
            if self.current_node < self.path.len() {
                self.next_position = self.path[self.current_node];
                self.current_node += 1;
                return NavigationStatus::InProgress;
            }

            // `nens.navigator().clearCachedWayPath()`.
            navigator.clear_cached_paths();

            // Figure out the source navloc (`createPathNavLoc`).
            let src_valid = navigator.clip_to_mesh(position, self.girth).is_some();

            // Complain if invalid.
            if !src_valid || self.dst_navloc.is_none() {
                return NavigationStatus::Failed;
            }

            let Some(next) = navigator.navigate_position(
                position,
                self.destination,
                self.max_distance,
                self.girth,
            ) else {
                return NavigationStatus::Failed;
            };
            self.next_position = next;

            // If we are still navigating, setup the state for the next
            // round of movement.
            self.current_node = 0;
            self.path.clear();
            self.generate_traversal_path(navigator, position);
        }

        NavigationStatus::InProgress
    }

    /// `startReal` (`navigation_controller.cpp:192-247`). `chunk_space`
    /// is `entity().pChunkSpace() != NULL`: a non-chunk (delegate) space
    /// warns and returns before registering, so the controller never
    /// updates. This is the restart path too — `Controllers::startReals`
    /// calls it with `isInitialStart` false, re-resolving the mesh from
    /// the streamed destination.
    pub fn start(
        &mut self,
        state: &mut EntityState,
        navigator: &mut dyn Navigator,
        chunk_space: bool,
    ) {
        //ToDo: remove when navigation system supports non-grid space types
        if !chunk_space {
            // WARNING_MSG( "NavigationController::startReal: "
            //     "Physical Space type of entity is not supported")
            self.warnings.push(
                "NavigationController::startReal: Physical Space type of entity is not supported",
            );
            return;
        }

        self.current_node = 0;
        self.path.clear();

        // Figure out the source navloc (`NavLoc( pChunkSpace(),
        // position(), girth_ )`) and the dest navloc. `clip_to_mesh`
        // folds the reference's `pDstLoc_->valid() && srcLoc.valid()`
        // checks.
        let src_valid = navigator.clip_to_mesh(state.position, self.girth).is_some();
        self.dst_navloc = navigator.clip_to_mesh(self.destination, self.girth);

        if self.dst_navloc.is_some() && src_valid {
            // `pDstLoc_->clip(); destination_ = pDstLoc_->point();` — the
            // clip face is inside `clip_to_mesh`.
            self.destination = self.dst_navloc.expect("checked above");

            navigator.clear_cached_paths();

            match navigator.navigate_position(
                state.position,
                self.destination,
                self.max_distance,
                self.girth,
            ) {
                None => self.next_position = state.position,
                Some(next) => {
                    self.next_position = next;
                    self.generate_traversal_path(navigator, state.position);
                }
            }
        } else {
            self.next_position = state.position;
        }

        // `CellApp::instance().registerForUpdate( this )`.
        self.registered = true;
    }

    /// `stopReal` (`navigation_controller.cpp:249-256`): deregister and
    /// drop the destination navloc.
    pub fn stop(&mut self) {
        self.registered = false;
        self.dst_navloc = None;
    }

    /// `Controller::cancel` (`Controllers::delController`).
    pub fn cancel(&mut self) {
        self.cancelled = true;
    }

    /// Whether `Controllers::delController` ran.
    pub fn is_cancelled(&self) -> bool {
        self.cancelled
    }

    /// `NavigationController::update` (`navigation_controller.cpp:283-313`):
    /// one tick — move, then fire the terminal callback and cancel. The
    /// `isAttached()` guards after the callbacks cannot re-enter in the
    /// port. Answers whether the controller cancelled itself.
    pub fn update(
        &mut self,
        state: &mut EntityState,
        navigator: &mut dyn Navigator,
        now: u32,
        hertz: f32,
    ) -> bool {
        let status = self.r#move(state, navigator, now, hertz);

        if status == NavigationStatus::Complete {
            // SCOPED_PROFILE( ON_NAVIGATE_PROFILE )
            self.events.push(ScriptCallback {
                entity_id: state.entity_id,
                method: "onNavigate",
                controller_id: self.id,
                user_arg: self.user_arg,
            });
            self.cancel();
        } else if status == NavigationStatus::Failed {
            self.events.push(ScriptCallback {
                entity_id: state.entity_id,
                method: "onNavigateFailed",
                controller_id: self.id,
                user_arg: self.user_arg,
            });
            self.cancel();
        }
        // else still moving to destination( = 0), or cancelled( = -2)

        self.cancelled
    }

    /// `generateTraversalPath` (`navigation_controller.cpp:319-336`): the
    /// cached waypoint path becomes the hop list; when source and
    /// destination share a waypoint set, the exact destination is
    /// appended. Note the reference quirk this preserves:
    /// `nextPosition_` is set to `path_[0]` but `currentNode_` stays at
    /// zero, so the first arrival re-assigns `path_[0]` before advancing.
    fn generate_traversal_path(&mut self, navigator: &mut dyn Navigator, source: Vector3) {
        self.path = navigator.cached_waypoint_path();

        if self.path.is_empty() {
            return;
        }

        self.next_position = self.path[0];

        let src_set = navigator.waypoint_set(source, self.girth);
        let dst_point = self.dst_navloc.expect("valid before navigating");
        if src_set.is_some() && src_set == navigator.waypoint_set(dst_point, self.girth) {
            self.path.push(self.destination);
        }
    }

    /// `writeRealToStream` (`navigation_controller.cpp:258-264`):
    /// `destination_ << metresPerTick_ << faceMovement_ << maxDistance_
    /// << girth_`.
    pub fn write_real_to_stream(&self, stream: &mut BinaryStream) {
        stream.write_f32(self.destination.x);
        stream.write_f32(self.destination.y);
        stream.write_f32(self.destination.z);
        stream.write_f32(self.metres_per_tick);
        stream.write_bool(self.face_movement);
        stream.write_f32(self.max_distance);
        stream.write_f32(self.girth);
    }

    /// `readRealFromStream` (`navigation_controller.cpp:266-281`): re-seed
    /// `nextPosition_` from the destination. Answers `Ok(false)` — the
    /// reference's `return false` — when the entity is not in a chunk
    /// space (the delegate-space migration face); the fields are consumed
    /// either way. The `pDstLoc_` re-creation folds into the `start`
    /// that `Controllers::startReals` runs next.
    pub fn read_real_from_stream(
        &mut self,
        stream: &mut BinaryStream,
        chunk_space: bool,
    ) -> Result<bool, StreamError> {
        let x = stream.read_f32()?;
        let y = stream.read_f32()?;
        let z = stream.read_f32()?;
        self.destination = Vector3::new(x, y, z);
        self.metres_per_tick = stream.read_f32()?;
        self.face_movement = stream.read_bool()?;
        self.max_distance = stream.read_f32()?;
        self.girth = stream.read_f32()?;
        self.next_position = self.destination;

        //ToDo: remove when navigation system supports non-grid space types
        if !chunk_space {
            // WARNING_MSG( "NavigationController::readRealFromStream: "
            //     "Physical Space type of entity is not supported")
            self.warnings.push(
                "NavigationController::readRealFromStream: Physical Space type of entity is not supported",
            );
            return Ok(false);
        }

        Ok(true)
    }
}

// -----------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use foundation::stream::BinaryStream;
    use std::collections::VecDeque;

    const HERTZ: f32 = crate::movement::DEFAULT_GAME_UPDATE_HERTZ;
    /// `CellAppConfig::maxAoIRadius()` stand-in for the tests.
    const MAX_AOI_RADIUS: f32 = 450.0;

    fn state_at(position: Vector3) -> EntityState {
        EntityState::new(7, position, crate::movement::Direction3D::default(), 0)
    }

    fn approx(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-4
    }

    fn approx_v(a: Vector3, b: Vector3) -> bool {
        approx(a.x, b.x) && approx(a.y, b.y) && approx(a.z, b.z)
    }

    /// A programmed navigator: `clip_to_mesh` refuses the exact points in
    /// `invalid`, `navigate_position` and `cached_waypoint_path` consume
    /// their queues in order (exhausted queues answer `None`/empty), and
    /// `waypoint_set` splits the world at `set_split_x`.
    struct StubNavigator {
        invalid: Vec<Vector3>,
        hops: VecDeque<Option<Vector3>>,
        paths: VecDeque<Vec<Vector3>>,
        set_split_x: f32,
        clears: usize,
    }

    impl StubNavigator {
        fn new() -> Self {
            Self {
                invalid: Vec::new(),
                hops: VecDeque::new(),
                paths: VecDeque::new(),
                set_split_x: 5.0,
                clears: 0,
            }
        }

        fn with_hop(mut self, hop: Option<Vector3>) -> Self {
            self.hops.push_back(hop);
            self
        }

        fn with_path(mut self, path: Vec<Vector3>) -> Self {
            self.paths.push_back(path);
            self
        }

        fn invalid_point(mut self, point: Vector3) -> Self {
            self.invalid.push(point);
            self
        }
    }

    impl Navigator for StubNavigator {
        fn clip_to_mesh(&mut self, point: Vector3, _girth: f32) -> Option<Vector3> {
            if self.invalid.contains(&point) {
                None
            } else {
                Some(point)
            }
        }

        fn navigate_position(
            &mut self,
            _source: Vector3,
            _destination: Vector3,
            _max_distance: f32,
            _girth: f32,
        ) -> Option<Vector3> {
            self.hops.pop_front().flatten()
        }

        fn waypoint_set(&mut self, point: Vector3, _girth: f32) -> Option<u64> {
            Some(if point.x < self.set_split_x { 1 } else { 2 })
        }

        fn cached_waypoint_path(&mut self) -> Vec<Vector3> {
            self.paths.pop_front().unwrap_or_default()
        }

        fn clear_cached_paths(&mut self) {
            self.clears += 1;
        }
    }

    // -------------------------------------------------------------------
    // TDD: BW-08-02-04 定时器与寻路控制器接续 — 退出或迁移后状态满足基线
    // -------------------------------------------------------------------

    /// The constructor: `-1` max distance resolves to the AOI radius, an
    /// explicit one is kept, and a tiny `closeEnough` warns.
    #[test]
    fn constructor_resolves_max_distance_and_warns_on_tiny_close_enough() {
        let controller = NavigationController::new(
            1,
            0,
            Vector3::new(10.0, 0.0, 0.0),
            10.0,
            true,
            -1.0,
            0.5,
            0.01,
            HERTZ,
            MAX_AOI_RADIUS,
        );
        assert!(controller.max_distance == MAX_AOI_RADIUS);
        assert!((controller.metres_per_tick - 1.0).abs() < 1e-6);
        assert!(controller.warnings.is_empty());

        let controller = NavigationController::new(
            1,
            0,
            Vector3::new(10.0, 0.0, 0.0),
            10.0,
            true,
            30.0,
            0.5,
            0.01,
            HERTZ,
            MAX_AOI_RADIUS,
        );
        assert_eq!(controller.max_distance, 30.0);

        let controller = NavigationController::new(
            1,
            0,
            Vector3::new(10.0, 0.0, 0.0),
            10.0,
            true,
            -1.0,
            0.5,
            0.0005,
            HERTZ,
            MAX_AOI_RADIUS,
        );
        assert_eq!(
            controller.warnings,
            vec!["NavigationController::NavigationController: closeEnough_ is very small"]
        );
    }

    /// A direct walk: the first `getNavigatePosition` lands on the
    /// destination itself, the entity steps one metre per tick, snaps the
    /// last half-metre, faces the movement, and finishes with `onNavigate`
    /// and a cancel.
    #[test]
    fn navigation_walks_a_direct_line_to_the_destination() {
        let mut controller = NavigationController::new(
            4,
            9,
            Vector3::new(5.5, 0.0, 0.0),
            10.0,
            true,
            -1.0,
            0.5,
            0.01,
            HERTZ,
            MAX_AOI_RADIUS,
        );
        let mut state = state_at(Vector3::ZERO);
        let mut navigator = StubNavigator::new().with_hop(Some(Vector3::new(5.5, 0.0, 0.0)));

        controller.start(&mut state, &mut navigator, true);
        assert!(controller.registered);
        assert!(approx_v(
            controller.next_position,
            Vector3::new(5.5, 0.0, 0.0)
        ));

        let mut now = 0u32;
        while !controller.update(&mut state, &mut navigator, now + 1, HERTZ) {
            now += 1;
            assert!(now < 100, "must arrive");
        }
        now += 1;

        // Five full metres in five ticks, tick 6 snaps the last 0.5.
        assert_eq!(now, 6);
        assert!(approx_v(state.position, Vector3::new(5.5, 0.0, 0.0)));
        assert!(!state.is_on_ground);
        assert!((state.direction.yaw - std::f32::consts::FRAC_PI_2).abs() < 1e-5);
        assert_eq!(
            controller.events,
            vec![ScriptCallback {
                entity_id: 7,
                method: "onNavigate",
                controller_id: 4,
                user_arg: 9,
            }]
        );
        assert!(controller.is_cancelled());
    }

    /// A waypointed walk: the traversal path feeds the hops, including the
    /// reference quirk that `path_[0]` is re-assigned once before
    /// `currentNode_` advances; the hop list exhausts, the navigator is
    /// re-queried for a second round, and when that runs dry too the
    /// controller fails with `onNavigateFailed`.
    #[test]
    fn navigation_walks_waypoints_then_fails_when_the_mesh_runs_out() {
        let hop1 = Vector3::new(2.0, 0.0, 0.0);
        let hop2 = Vector3::new(4.0, 0.0, 0.0);
        let hop3 = Vector3::new(6.0, 0.0, 0.0);
        let mut controller = NavigationController::new(
            4,
            9,
            Vector3::new(10.0, 0.0, 0.0),
            10.0,
            false,
            -1.0,
            0.5,
            0.01,
            HERTZ,
            MAX_AOI_RADIUS,
        );
        let mut state = state_at(Vector3::ZERO);
        // Source left of the split, destination right of it: the sets
        // differ, so the exact destination is not appended. The second
        // round (after hop2) lands on hop3 with a fresh traversal path;
        // the third query comes up empty.
        let mut navigator = StubNavigator::new()
            .with_hop(Some(hop1))
            .with_path(vec![hop1, hop2])
            .with_hop(Some(hop3))
            .with_path(vec![hop3]);

        controller.start(&mut state, &mut navigator, true);
        // `generateTraversalPath` pins the first hop.
        assert!(approx_v(controller.next_position, hop1));

        // Tick 1: reach hop1 (2 m > 1 m/tick takes 2 ticks: tick 1 walks
        // to x=1, tick 2 snaps x=2); the arrival re-assigns path_[0].
        assert!(!controller.update(&mut state, &mut navigator, 1, HERTZ));
        assert!((state.position.x - 1.0).abs() < 1e-5);
        assert!(!controller.update(&mut state, &mut navigator, 2, HERTZ));
        assert!(approx_v(state.position, hop1));
        assert!(approx_v(controller.next_position, hop1), "path_[0] again");

        // Tick 3: the same position re-snaps (movement is zero) and
        // advances to path_[1].
        assert!(!controller.update(&mut state, &mut navigator, 3, HERTZ));
        assert!(approx_v(controller.next_position, hop2));

        // Ticks 4-5: walk to hop2; the arrival exhausts the path and the
        // navigator is re-queried — the second round lands on hop3.
        assert!(!controller.update(&mut state, &mut navigator, 4, HERTZ));
        assert!(!controller.update(&mut state, &mut navigator, 5, HERTZ));
        assert!(approx_v(state.position, hop2));
        assert!(approx_v(controller.next_position, hop3), "second round");

        // Ticks 6-8: walk to hop3, re-assign, snap — then the third query
        // runs dry -> Failed on the same tick.
        assert!(!controller.update(&mut state, &mut navigator, 6, HERTZ));
        assert!(!controller.update(&mut state, &mut navigator, 7, HERTZ));
        assert!(approx_v(state.position, hop3));
        assert!(controller.update(&mut state, &mut navigator, 8, HERTZ));
        assert_eq!(
            controller.events,
            vec![ScriptCallback {
                entity_id: 7,
                method: "onNavigateFailed",
                controller_id: 4,
                user_arg: 9,
            }]
        );
        assert!(controller.is_cancelled());
        // One clear at start, one per exhausted round.
        assert_eq!(navigator.clears, 3, "clearCachedWayPath faces");
    }

    /// The vertical faces: an upward hop pins `position.y` to the hop's
    /// height every tick, and a downward hop never walks down — the
    /// movement's y is clamped to zero.
    #[test]
    fn navigation_never_walks_down_and_snaps_up() {
        let mut controller = NavigationController::new(
            1,
            0,
            Vector3::new(10.0, 3.0, 0.0),
            10.0,
            false,
            -1.0,
            0.5,
            0.01,
            HERTZ,
            MAX_AOI_RADIUS,
        );
        let mut state = state_at(Vector3::ZERO);
        let mut navigator = StubNavigator::new().with_hop(Some(Vector3::new(10.0, 3.0, 0.0)));
        controller.start(&mut state, &mut navigator, true);

        // Tick 1: the step (about 0.96 x, 0.29 y) rides below the hop's
        // height, so y pins to 3.
        controller.r#move(&mut state, &mut navigator, 1, HERTZ);
        assert_eq!(state.position.y, 3.0);
        assert!(state.position.x < 1.0);

        // A hop below the entity: movement.y clamps to zero, the walk
        // stays level.
        controller.next_position = Vector3::new(20.0, -5.0, 0.0);
        controller.r#move(&mut state, &mut navigator, 2, HERTZ);
        assert_eq!(state.position.y, 3.0);
        assert!(state.position.x > 1.0);
    }

    /// An invalid destination (or an invalid source) cannot seed a hop:
    /// the start keeps the entity where it is, and the first tick reports
    /// `onNavigateFailed`.
    #[test]
    fn an_unresolvable_start_fails_on_the_first_tick() {
        let destination = Vector3::new(10.0, 0.0, 0.0);

        // Invalid destination.
        let mut controller = NavigationController::new(
            2,
            0,
            destination,
            10.0,
            false,
            -1.0,
            0.5,
            0.01,
            HERTZ,
            MAX_AOI_RADIUS,
        );
        let mut state = state_at(Vector3::ZERO);
        let mut navigator = StubNavigator::new().invalid_point(destination);
        controller.start(&mut state, &mut navigator, true);
        assert!(controller.registered, "registered even with no route");
        assert!(approx_v(controller.next_position, state.position));
        assert!(controller.dst_navloc.is_none());

        assert!(controller.update(&mut state, &mut navigator, 1, HERTZ));
        assert_eq!(controller.events.len(), 1);
        assert_eq!(controller.events[0].method, "onNavigateFailed");
        assert_eq!(navigator.clears, 1);

        // Invalid source: the same else-branch face.
        let mut controller = NavigationController::new(
            2,
            0,
            destination,
            10.0,
            false,
            -1.0,
            0.5,
            0.01,
            HERTZ,
            MAX_AOI_RADIUS,
        );
        let mut state = state_at(Vector3::ZERO);
        let mut navigator = StubNavigator::new()
            .invalid_point(Vector3::ZERO)
            .with_hop(Some(destination));
        controller.start(&mut state, &mut navigator, true);
        assert!(approx_v(controller.next_position, state.position));
        assert!(controller.dst_navloc.is_some());

        assert!(controller.update(&mut state, &mut navigator, 1, HERTZ));
        assert_eq!(controller.events[0].method, "onNavigateFailed");

        // Both ends valid but no route: the start still keeps the entity
        // in place.
        let mut controller = NavigationController::new(
            2,
            0,
            destination,
            10.0,
            false,
            -1.0,
            0.5,
            0.01,
            HERTZ,
            MAX_AOI_RADIUS,
        );
        let mut state = state_at(Vector3::ZERO);
        let mut navigator = StubNavigator::new();
        controller.start(&mut state, &mut navigator, true);
        assert!(approx_v(controller.next_position, state.position));
        assert!(controller.dst_navloc.is_some());

        assert!(controller.update(&mut state, &mut navigator, 1, HERTZ));
        assert_eq!(controller.events[0].method, "onNavigateFailed");
    }

    /// A waypoint set shared with the destination appends the exact
    /// destination to the traversal path, so the walk finishes exactly on
    /// it.
    #[test]
    fn a_shared_waypoint_set_appends_the_exact_destination() {
        let hop1 = Vector3::new(1.5, 0.0, 0.0);
        let destination = Vector3::new(2.5, 0.0, 0.0);
        let mut controller = NavigationController::new(
            3,
            0,
            destination,
            10.0,
            false,
            -1.0,
            0.5,
            0.01,
            HERTZ,
            MAX_AOI_RADIUS,
        );
        let mut state = state_at(Vector3::ZERO);
        // Source (x=0) and destination (x=2.5) both left of the split at
        // 5: one set, so the destination is appended.
        let mut navigator = StubNavigator::new()
            .with_hop(Some(hop1))
            .with_path(vec![hop1]);

        controller.start(&mut state, &mut navigator, true);
        assert!(approx_v(controller.next_position, hop1));

        let mut now = 0;
        while !controller.update(&mut state, &mut navigator, now + 1, HERTZ) {
            now += 1;
            assert!(now < 100, "must arrive");
        }
        assert!(approx_v(state.position, destination));
        assert_eq!(controller.events[0].method, "onNavigate");
    }

    /// The offload round-trip: state streams as the five fields, the
    /// restore re-seeds `nextPosition_` from the destination, and the
    /// restart re-resolves the mesh and walks to completion — the
    /// "迁移后状态满足基线" face.
    #[test]
    fn offload_restart_re_resolves_the_mesh_and_finishes() {
        let destination = Vector3::new(10.0, 0.0, 0.0);
        let mut controller = NavigationController::new(
            4,
            9,
            destination,
            10.0,
            false,
            -1.0,
            0.5,
            0.01,
            HERTZ,
            MAX_AOI_RADIUS,
        );
        let mut state = state_at(Vector3::ZERO);
        let mut navigator = StubNavigator::new().with_hop(Some(destination));
        controller.start(&mut state, &mut navigator, true);

        // Walk two ticks, then offload: `Controllers::stopReals` runs
        // `stopReal` (deregister, drop the navloc) before the stream.
        assert!(!controller.update(&mut state, &mut navigator, 1, HERTZ));
        assert!(!controller.update(&mut state, &mut navigator, 2, HERTZ));
        assert!((state.position.x - 2.0).abs() < 1e-5);
        controller.stop();
        assert!(!controller.registered);
        assert!(controller.dst_navloc.is_none());

        let mut stream = BinaryStream::new();
        controller.write_real_to_stream(&mut stream);
        stream.rewind();

        let mut restored = NavigationController::new(
            4,
            9,
            Vector3::ZERO,
            0.0,
            true,
            -1.0,
            1.0,
            1.0,
            HERTZ,
            MAX_AOI_RADIUS,
        );
        assert!(restored.read_real_from_stream(&mut stream, true).unwrap());
        assert!(approx_v(restored.destination, destination));
        assert!((restored.metres_per_tick - 1.0).abs() < 1e-6);
        assert!(!restored.face_movement);
        assert_eq!(restored.max_distance, MAX_AOI_RADIUS);
        assert!((restored.girth - 0.5).abs() < 1e-6);
        // The re-seed: the restore walks at the destination before the
        // restart resolves the mesh.
        assert!(approx_v(restored.next_position, destination));

        // Restart on the new cell: the mesh resolves again and the walk
        // completes from the restored position.
        let mut navigator = StubNavigator::new().with_hop(Some(destination));
        restored.start(&mut state, &mut navigator, true);
        assert!(approx_v(restored.next_position, destination));
        assert!(restored.registered);
        assert!(!restored.is_cancelled());

        let mut now = 2;
        while !restored.update(&mut state, &mut navigator, now + 1, HERTZ) {
            now += 1;
            assert!(now < 100, "must arrive");
        }
        assert!(approx_v(state.position, destination));
        assert_eq!(restored.events[0].method, "onNavigate");
    }

    /// A migration into a non-chunk (delegate) space: the restore refuses
    /// (`return false`) with the reference's warning, and the start
    /// refuses to register — the controller stays a silent baseline.
    #[test]
    fn a_delegate_space_migration_refuses_the_controller() {
        let controller = NavigationController::new(
            4,
            9,
            Vector3::new(10.0, 0.0, 0.0),
            10.0,
            false,
            -1.0,
            0.5,
            0.01,
            HERTZ,
            MAX_AOI_RADIUS,
        );
        let mut stream = BinaryStream::new();
        controller.write_real_to_stream(&mut stream);
        stream.rewind();

        let mut restored = NavigationController::new(
            4,
            9,
            Vector3::ZERO,
            0.0,
            true,
            -1.0,
            1.0,
            1.0,
            HERTZ,
            MAX_AOI_RADIUS,
        );
        assert!(!restored.read_real_from_stream(&mut stream, false).unwrap());
        assert_eq!(
            restored.warnings,
            vec![
                "NavigationController::readRealFromStream: Physical Space type of entity is not supported"
            ]
        );

        let mut state = state_at(Vector3::ZERO);
        let mut navigator = StubNavigator::new();
        restored.start(&mut state, &mut navigator, false);
        assert!(!restored.registered, "never updates in a non-chunk space");
        assert_eq!(
            restored.warnings[1],
            "NavigationController::startReal: Physical Space type of entity is not supported"
        );
        assert_eq!(navigator.clears, 0);

        // The driver loop never ticks it: no callbacks ever fire.
        assert!(!restored.is_cancelled());
        assert!(restored.events.is_empty());
    }

    /// A truncated stream refuses.
    #[test]
    fn the_stream_roundtrip_refuses_truncation() {
        let controller = NavigationController::new(
            1,
            0,
            Vector3::new(10.0, 0.0, 0.0),
            10.0,
            true,
            -1.0,
            0.5,
            0.01,
            HERTZ,
            MAX_AOI_RADIUS,
        );
        let mut stream = BinaryStream::new();
        controller.write_real_to_stream(&mut stream);
        stream.rewind();
        let mut restored = NavigationController::new(
            1,
            0,
            Vector3::ZERO,
            0.0,
            true,
            -1.0,
            1.0,
            1.0,
            HERTZ,
            MAX_AOI_RADIUS,
        );
        restored.read_real_from_stream(&mut stream, true).unwrap();
        assert!(stream.at_end());

        // A truncated stream reports the error.
        let mut bad = BinaryStream::new();
        bad.write_f32(1.0);
        bad.rewind();
        assert!(restored.read_real_from_stream(&mut bad, true).is_err());
    }
}
