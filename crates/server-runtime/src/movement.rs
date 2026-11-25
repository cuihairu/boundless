//! The movement, turning and acceleration controllers
//! (`server/cellapp/move_controller.*`, `turn_controller.*`,
//! `base_acceleration_controller.*`, `accelerate_to_point_controller.*`,
//! `accelerate_along_path_controller.*`,
//! `accelerate_to_entity_controller.*`).
//!
//! All three families drive the entity from `update()` once per game tick
//! (`CellApp::registerForUpdate` — the tick loop calls these in registration
//! order) and all register as DOMAIN_REAL controllers in the exclusive
//! `"Movement"` category, so starting one cancels the previous one
//! ([`crate::controllers`]). The acceptance for BW-08-02-03 is
//! "固定输入轨迹吻合": with fixed inputs the per-tick positions, directions
//! and callbacks reproduce the reference algorithms step for step.
//!
//! The float faces are pinned to the reference scalars: `metresPerTick_ =
//! velocity / updateHertz` (`move_controller.cpp:107-109`), the arrival
//! tests are `almostEqual(..., 0.0004)` (`stdmf.hpp:606-609`), the axis
//! guards are exact `isZero` (`stdmf_minimal.hpp:113-141`), yaw is
//! `atan2f(x, z)` and pitch `atan2f(-y, sqrtf(x²+z²))` (`vector3.ipp`), and
//! `Angle` keeps values wrapped into `(-π, π]` (`angle.ipp:124-158`).
//!
//! Deliberately deferred: the vehicle branches (`Passengers::
//! transformToVehiclePos/Dir`) land with BW-08-02-05, and the navigation
//! registry hooks behind `NavigateStepController::startReal` land with
//! BW-08-02-04 — the controller's own update loop is ported here.

use foundation::math::{Vector3, almost_zero};

use crate::controllers::ControllerId;
use crate::entity_dispatch::EntityId;

/// `DEFAULT_GAME_UPDATE_HERTZ` (`lib/server/common.hpp:55`).
pub const DEFAULT_GAME_UPDATE_HERTZ: f32 = 10.0;

/// `MATH_PI` (`mathdef.hpp`).
pub const MATH_PI: f32 = std::f32::consts::PI;

/// `Entity::INVALID_POSITION`.
pub const INVALID_POSITION: Vector3 = Vector3::new(f32::MAX, f32::MAX, f32::MAX);

/// `BaseAccelerationController::Facing` (`base_acceleration_controller.hpp:31-40`).
pub const FACING_NONE: u8 = 0;
/// Face along the velocity while moving.
pub const FACING_VELOCITY: u8 = 1;
/// Face along the acceleration while it applies.
pub const FACING_ACCELERATION: u8 = 2;
/// One past the last valid facing — the script faces validate
/// `0..FACING_MAX`.
pub const FACING_MAX: u8 = 3;

// -----------------------------------------------------------------------------
// Section: The exact scalar helpers (`stdmf*.hpp`, `angle.ipp`)
// -----------------------------------------------------------------------------

/// `isZero(v)` — `isEqual(v, 0)` and float `isEqual` is `fabsf(f1-f2) <= 0`:
/// an exact zero test (`stdmf_minimal.hpp:113-141`).
fn is_zero(value: f32) -> bool {
    value == 0.0
}

/// `almostEqual(f1, f2)` (`stdmf.hpp:606-609`).
fn almost_equal(f1: f32, f2: f32) -> bool {
    (f1 - f2).abs() < 0.0004
}

/// `almostEqual(v1, v2)` component-wise (`vector3.hpp:130-136`).
fn vectors_almost_equal(v1: Vector3, v2: Vector3) -> bool {
    almost_equal(v1.x, v2.x) && almost_equal(v1.y, v2.y) && almost_equal(v1.z, v2.z)
}

/// `Vector3::unitVector` (`vector3.ipp:288-296`): a normalised copy; the
/// zero vector stays zero.
fn unit_vector(v: Vector3) -> Vector3 {
    let mut unit = v;
    unit.normalise();
    unit
}

/// `Angle::normalise` (`angle.ipp:124-158`): wrap into `(-π, π]`.
fn angle_normalise(value: f32) -> f32 {
    let two_pi = MATH_PI * 2.0;
    let three_pi = MATH_PI * 3.0;

    if value > MATH_PI {
        if value > three_pi {
            value - ((value + MATH_PI) / two_pi).floor() * two_pi
        } else {
            value - two_pi
        }
    } else if value <= -MATH_PI {
        if value <= -three_pi {
            value - ((value + MATH_PI) / two_pi).floor() * two_pi
        } else {
            value + two_pi
        }
    } else {
        value
    }
}

/// `Angle::turnRange(from, to)` (`angle.ipp:279-284`): the clockwise range
/// `from -> to` in `[0, 2π)`.
fn turn_range(from: f32, to: f32) -> f32 {
    let diff = to - from;
    if diff < 0.0 {
        diff + MATH_PI * 2.0
    } else {
        diff
    }
}

/// `Angle::isBetween(min, max)` (`angle.ipp:244-250`): whether `value` lies
/// on the clockwise arc `min -> max`.
fn angle_is_between(value: f32, min: f32, max: f32) -> bool {
    turn_range(min, value) <= turn_range(min, max)
}

// -----------------------------------------------------------------------------
// Section: The entity motion state (`Entity`'s local transform and
// `RealEntity`'s velocity sample)
// -----------------------------------------------------------------------------

/// `(yaw, pitch, roll)` in radians (`Direction3D`).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Direction3D {
    /// `direction.yaw`.
    pub yaw: f32,
    /// `direction.pitch`.
    pub pitch: f32,
    /// `direction.roll`.
    pub roll: f32,
}

impl Direction3D {
    /// `Direction3D( yaw, pitch, roll )`.
    pub fn new(yaw: f32, pitch: f32, roll: f32) -> Self {
        Self { yaw, pitch, roll }
    }
}

/// The slice of entity state the movement controllers read and write:
/// `Entity::localPosition_/localDirection_` plus the
/// `RealEntity` velocity sample (`real_entity.cpp:829-856`) that the
/// acceleration controllers consume on their next tick. The caller owns
/// the entity; a controller mutates this through
/// [`EntityState::set_position_and_direction`], which is where the
/// reference's `setPositionAndDirection` -> `updateGlobalPosition` ->
/// `RealEntity::newPosition` sampling lands.
#[derive(Clone, Debug)]
pub struct EntityState {
    /// `Entity::id()`.
    pub entity_id: EntityId,
    /// `localPosition_`.
    pub position: Vector3,
    /// `localDirection_`.
    pub direction: Direction3D,
    /// `isOnGround_` — the controllers clear it on the first move
    /// (`move_controller.cpp:197-199`).
    pub is_on_ground: bool,
    /// `pReal_->velocity()` — refreshed by the position samples below.
    pub velocity: Vector3,
    /// `positionSample_` / `positionSampleTime_` (`real_entity.hpp`).
    position_sample: Vector3,
    /// The tick of the last sample.
    position_sample_time: u32,
}

impl EntityState {
    /// A state seeded like a fresh `RealEntity` (`real_entity.cpp:115-140`):
    /// the sample starts at the creation position, sampled at `now`.
    pub fn new(entity_id: EntityId, position: Vector3, direction: Direction3D, now: u32) -> Self {
        Self {
            entity_id,
            position,
            direction,
            is_on_ground: true,
            velocity: Vector3::ZERO,
            position_sample: position,
            position_sample_time: now,
        }
    }

    /// `RealEntity::newPosition` (`real_entity.cpp:830-856`), driven from
    /// every position set: after more than one tick, the velocity is the
    /// sample-to-now delta scaled by `updateHertz / dur`.
    pub fn set_position_and_direction(
        &mut self,
        position: Vector3,
        direction: Direction3D,
        now: u32,
        hertz: f32,
    ) {
        self.position = position;
        self.direction = direction;

        let dur = now - self.position_sample_time;
        if dur > 1 {
            if self.position_sample == INVALID_POSITION {
                self.position_sample = position;
            }
            let tinv = hertz / dur as f32;
            self.velocity = (position - self.position_sample) * tinv;
            self.position_sample = position;
            self.position_sample_time = now;
        }
    }
}

/// One `Controller::standardCallback` (`controller.cpp:139-160`): the
/// named script callback with `(controllerID, userArg)`, recorded in
/// firing order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScriptCallback {
    /// `entity().id()`.
    pub entity_id: EntityId,
    /// The callback name.
    pub method: &'static str,
    /// `controllerID_`.
    pub controller_id: ControllerId,
    /// `userArg_`.
    pub user_arg: i32,
}

// -----------------------------------------------------------------------------
// Section: MoveController (`move_controller.hpp:29-70`)
// -----------------------------------------------------------------------------

/// The `MoveController` base: move towards a destination at a fixed
/// metres-per-second rate (`move_controller.cpp:100-209`).
#[derive(Clone, Debug)]
pub struct MoveControllerCore {
    /// `metresPerTick_ = velocity / updateHertz` (`move_controller.cpp:109`).
    pub metres_per_tick: f32,
    /// `faceMovement_`.
    pub face_movement: bool,
    /// `moveVertically_`.
    pub move_vertically: bool,
    /// `CellAppConfig::shouldNavigationDropPosition` (default false,
    /// `cellapp_config.cpp:73`).
    pub navigation_drop_position: bool,
    /// `CellAppConfig::navigationMaxClimb` (`cellapp_config.cpp:79`).
    pub navigation_max_climb: f32,
    /// `CellAppConfig::navigationMaxSlope` in degrees
    /// (`cellapp_config.cpp:128-130` takes `tan` of it).
    pub navigation_max_slope_degrees: f32,
    /// The update hertz this controller was built with.
    pub hertz: f32,
}

impl MoveControllerCore {
    /// `MoveController::MoveController` (`move_controller.cpp:105-110`).
    pub fn new(velocity: f32, face_movement: bool, move_vertically: bool, hertz: f32) -> Self {
        Self {
            metres_per_tick: velocity / hertz,
            face_movement,
            move_vertically,
            navigation_drop_position: false,
            navigation_max_climb: 0.65,
            navigation_max_slope_degrees: 45.0,
            hertz,
        }
    }

    /// `MoveController::move` (`move_controller.cpp:118-209`): advance the
    /// entity towards `destination` by `metresPerTick_` (or the remaining
    /// tick budget's worth), face the movement, clear `isOnGround`, and
    /// answer whether the destination was reached. `remaining_ticks` is
    /// `NavigateStepController`'s leftover-tick budget: consumed here,
    /// refunded on arrival.
    pub fn move_towards(
        &mut self,
        state: &mut EntityState,
        destination: Vector3,
        now: u32,
        remaining_ticks: Option<&mut f32>,
    ) -> bool {
        let mut position = state.position;
        let mut direction = state.direction;
        let mut metres_to_go = self.metres_per_tick;

        if let Some(ticks) = remaining_ticks.as_deref() {
            metres_to_go = *ticks * self.metres_per_tick;
        }

        let mut movement = destination - position;
        if !self.move_vertically {
            movement.y = 0.0;
        }

        // If we are real close, then finish up.
        if movement.length() < metres_to_go {
            position = destination;
            if let Some(ticks) = remaining_ticks {
                *ticks *= (metres_to_go - movement.length()) / self.metres_per_tick;
            }
        } else {
            movement.normalise();
            movement = movement * metres_to_go;
            position = position + movement;

            if let Some(ticks) = remaining_ticks {
                *ticks = 0.0;
            }
        }

        // Make sure we are facing the right direction.
        if self.face_movement {
            if !is_zero(movement.x) || !is_zero(movement.z) {
                direction.yaw = movement.yaw();
            }
            if !is_zero(movement.y) {
                direction.pitch = movement.pitch();
            }
        }

        if self.navigation_drop_position {
            let slope = self.navigation_max_slope_degrees.to_radians().tan();
            position.y += self.navigation_max_climb + movement.length() * slope;
        }

        // No longer on the ground...
        state.is_on_ground = false;
        state.set_position_and_direction(position, direction, now, self.hertz);

        almost_equal(position.x, destination.x)
            && almost_equal(position.z, destination.z)
            && (almost_equal(position.y, destination.y) || !self.move_vertically)
    }
}

/// `MoveToPointController` (`move_controller.hpp:73-91`): walk to a fixed
/// point, `onMove` and cancel on arrival.
#[derive(Clone, Debug)]
pub struct MoveToPointController {
    /// The controller id from `Controllers::add_controller`.
    pub id: ControllerId,
    /// The script's user data.
    pub user_arg: i32,
    /// The `MoveController` base.
    pub core: MoveControllerCore,
    /// `destination_`.
    pub destination: Vector3,
}

impl MoveToPointController {
    /// The constructor (`move_controller.cpp:245-256`).
    pub fn new(
        id: ControllerId,
        user_arg: i32,
        destination: Vector3,
        velocity: f32,
        face_movement: bool,
        move_vertically: bool,
        hertz: f32,
    ) -> Self {
        Self {
            id,
            user_arg,
            core: MoveControllerCore::new(velocity, face_movement, move_vertically, hertz),
            destination,
        }
    }

    /// `MoveToPointController::update` (`move_controller.cpp:287-303`).
    /// Answers true when the controller cancelled itself (arrival).
    pub fn update(
        &mut self,
        state: &mut EntityState,
        now: u32,
        log: &mut Vec<ScriptCallback>,
    ) -> bool {
        if !self.core.move_towards(state, self.destination, now, None) {
            return false;
        }
        log.push(ScriptCallback {
            entity_id: state.entity_id,
            method: "onMove",
            controller_id: self.id,
            user_arg: self.user_arg,
        });
        true
    }

    /// `writeRealToStream` (`move_controller.cpp:213-218` plus the
    /// subclass fields): metres per tick, the two movement flags,
    /// then the destination.
    pub fn write_real_to_stream(&self, stream: &mut foundation::stream::BinaryStream) {
        stream.write_f32(self.core.metres_per_tick);
        stream.write_bool(self.core.face_movement);
        stream.write_bool(self.core.move_vertically);
        stream.write_f32(self.destination.x);
        stream.write_f32(self.destination.y);
        stream.write_f32(self.destination.z);
    }

    /// `readRealFromStream` (`move_controller.cpp:221-227`).
    pub fn read_real_from_stream(
        &mut self,
        stream: &mut foundation::stream::BinaryStream,
    ) -> Result<(), foundation::stream::StreamError> {
        self.core.metres_per_tick = stream.read_f32()?;
        self.core.face_movement = stream.read_bool()?;
        self.core.move_vertically = stream.read_bool()?;
        self.destination = Vector3::from_stream_bytes(stream)?;
        Ok(())
    }
}

/// `NavigateStepController` (`move_controller.hpp:94-117`): the
/// `navigateStep` face — chain waypoints through `onMove`, spending the
/// leftover tick fraction of each arrival on the next leg.
#[derive(Clone, Debug)]
pub struct NavigateStepController {
    /// The controller id.
    pub id: ControllerId,
    /// The script's user data.
    pub user_arg: i32,
    /// The `MoveController` base.
    pub core: MoveControllerCore,
    /// `destination_`.
    pub destination: Vector3,
}

impl NavigateStepController {
    /// The constructor (`move_controller.cpp:312-326`).
    pub fn new(
        id: ControllerId,
        user_arg: i32,
        destination: Vector3,
        velocity: f32,
        face_movement: bool,
        hertz: f32,
    ) -> Self {
        Self {
            id,
            user_arg,
            core: MoveControllerCore::new(velocity, face_movement, false, hertz),
            destination,
        }
    }

    /// `setDestination` (`move_controller.cpp:349-352`) — script's next
    /// waypoint from inside `onMove`.
    pub fn set_destination(&mut self, destination: Vector3) {
        self.destination = destination;
    }

    /// `NavigateStepController::update` (`move_controller.cpp:375-424`):
    /// chain legs while tick budget remains, one `onMove` per waypoint.
    /// `on_move` models the script's synchronous re-navigation inside the
    /// callback — it returns the next waypoint, or `None` when the script
    /// navigated nowhere new (the cancel face). The 100-loop guard turns
    /// into `onMoveFailure` and a cancel. Answers true when the
    /// controller cancelled itself.
    pub fn update(
        &mut self,
        state: &mut EntityState,
        now: u32,
        on_move: &mut dyn FnMut() -> Option<Vector3>,
        log: &mut Vec<ScriptCallback>,
    ) -> bool {
        let mut tick = 1.0f32;
        let mut num_of_loops = 0;

        while !almost_zero(tick, 0.0004)
            && self
                .core
                .move_towards(state, self.destination, now, Some(&mut tick))
        {
            self.destination = INVALID_POSITION;

            num_of_loops += 1;
            if num_of_loops < 100 {
                log.push(ScriptCallback {
                    entity_id: state.entity_id,
                    method: "onMove",
                    controller_id: self.id,
                    user_arg: self.user_arg,
                });
            } else {
                log.push(ScriptCallback {
                    entity_id: state.entity_id,
                    method: "onMoveFailure",
                    controller_id: self.id,
                    user_arg: self.user_arg,
                });
                return true;
            }

            if let Some(next) = on_move() {
                self.destination = next;
            }

            if self.destination == INVALID_POSITION {
                // The script navigated nowhere new: cancel
                // (move_controller.cpp:417-420).
                return true;
            }
        }
        false
    }

    /// `writeRealToStream` (`move_controller.cpp:356-363`).
    pub fn write_real_to_stream(&self, stream: &mut foundation::stream::BinaryStream) {
        stream.write_f32(self.core.metres_per_tick);
        stream.write_bool(self.core.face_movement);
        stream.write_bool(self.core.move_vertically);
        stream.write_f32(self.destination.x);
        stream.write_f32(self.destination.y);
        stream.write_f32(self.destination.z);
    }

    /// `readRealFromStream` (`move_controller.cpp:366-377`).
    pub fn read_real_from_stream(
        &mut self,
        stream: &mut foundation::stream::BinaryStream,
    ) -> Result<(), foundation::stream::StreamError> {
        self.core.metres_per_tick = stream.read_f32()?;
        self.core.face_movement = stream.read_bool()?;
        self.core.move_vertically = stream.read_bool()?;
        self.destination = Vector3::from_stream_bytes(stream)?;
        Ok(())
    }
}

/// `MoveToEntityController` (`move_controller.hpp:120-141`): approach a
/// live entity, stopping inside `range`; the destination entity is
/// resolved by the caller each tick.
#[derive(Clone, Debug)]
pub struct MoveToEntityController {
    /// The controller id.
    pub id: ControllerId,
    /// The script's user data.
    pub user_arg: i32,
    /// The `MoveController` base.
    pub core: MoveControllerCore,
    /// `destEntityID_`.
    pub dest_entity_id: EntityId,
    /// `range_` — stop once this close.
    pub range: f32,
    /// `offset_`, a random point within range of the destination.
    pub offset: Vector3,
    /// Whether the destination entity has been resolved (the reference
    /// caches the `EntityPtr` and picks the offset once).
    resolved: bool,
}

impl MoveToEntityController {
    /// The constructor (`move_controller.cpp:466-479`).
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id: ControllerId,
        user_arg: i32,
        dest_entity_id: EntityId,
        velocity: f32,
        range: f32,
        face_movement: bool,
        move_vertically: bool,
        hertz: f32,
    ) -> Self {
        Self {
            id,
            user_arg,
            core: MoveControllerCore::new(velocity, face_movement, move_vertically, hertz),
            dest_entity_id,
            range,
            offset: Vector3::ZERO,
            resolved: false,
        }
    }

    /// `MoveToEntityController::recalcOffset`
    /// (`move_controller.cpp:526-532`): a random bearing, full range out.
    /// `rand() % 360` becomes `rng % 360` so tests pin it.
    fn recalc_offset(&mut self, rng: u64) {
        let angle = (rng % 360) as f32 * (MATH_PI / 180.0);
        self.offset.x = self.range * angle.cos();
        self.offset.z = self.range * angle.sin();
    }

    /// `MoveToEntityController::update` (`move_controller.cpp:535-612`).
    /// `target` is the resolved destination entity's current position —
    /// `None` models the destroyed/missing target. Answers true when the
    /// controller cancelled itself.
    pub fn update(
        &mut self,
        state: &mut EntityState,
        target: Option<Vector3>,
        rng: u64,
        now: u32,
        log: &mut Vec<ScriptCallback>,
    ) -> bool {
        if !self.resolved {
            self.resolved = true;
            self.recalc_offset(rng);
        }

        let Some(mut destination) = target else {
            log.push(ScriptCallback {
                entity_id: state.entity_id,
                method: "onMoveFailure",
                controller_id: self.id,
                user_arg: self.user_arg,
            });
            return true;
        };

        // 2D remaining distance when not moving vertically; the
        // destination takes our height so the walk stays level
        // (move_controller.cpp:559-567).
        let mut remaining = destination - state.position;
        if !self.core.move_vertically {
            remaining.y = 0.0;
            destination.y = state.position.y;
        }
        let close_enough = remaining.length() < self.range;

        let dest_local = destination + self.offset;

        if close_enough || self.core.move_towards(state, dest_local, now, None) {
            if close_enough {
                // In case we were moving towards a point near the entity,
                // make sure that we are now facing it exactly
                // (move_controller.cpp:585-596).
                let vector = destination - state.position;
                if self.core.face_movement {
                    state.direction.yaw = vector.yaw();
                }
            }

            log.push(ScriptCallback {
                entity_id: state.entity_id,
                method: "onMove",
                controller_id: self.id,
                user_arg: self.user_arg,
            });
            return true;
        }
        false
    }

    /// `writeRealToStream` (`move_controller.cpp:483-489`).
    pub fn write_real_to_stream(&self, stream: &mut foundation::stream::BinaryStream) {
        stream.write_f32(self.core.metres_per_tick);
        stream.write_bool(self.core.face_movement);
        stream.write_bool(self.core.move_vertically);
        stream.write_u32(self.dest_entity_id);
        stream.write_f32(self.range);
    }

    /// `readRealFromStream` (`move_controller.cpp:492-499`).
    pub fn read_real_from_stream(
        &mut self,
        stream: &mut foundation::stream::BinaryStream,
    ) -> Result<(), foundation::stream::StreamError> {
        self.core.metres_per_tick = stream.read_f32()?;
        self.core.face_movement = stream.read_bool()?;
        self.core.move_vertically = stream.read_bool()?;
        self.dest_entity_id = stream.read_u32()?;
        self.range = stream.read_f32()?;
        self.resolved = false;
        Ok(())
    }
}

// -----------------------------------------------------------------------------
// Section: YawRotatorController (`turn_controller.*`)
// -----------------------------------------------------------------------------

/// `YawRotatorController` (`turn_controller.hpp:25-50`): turn the entity's
/// yaw at a fixed radians-per-second towards a target; the shortest
/// direction is chosen at start; `onTurn` fires and the controller
/// cancels on arrival.
#[derive(Clone, Debug)]
pub struct YawRotatorController {
    /// The controller id.
    pub id: ControllerId,
    /// The script's user data.
    pub user_arg: i32,
    /// `radiansPerTick_ = velocity / updateHertz` — negative once the
    /// anti-clockwise start picks the shorter way round.
    pub radians_per_tick: f32,
    /// `targetYaw_`, normalised into `(-π, π]`.
    pub target_yaw: f32,
}

impl YawRotatorController {
    /// `YawRotatorController::New` (`turn_controller.cpp:76-92`): a
    /// non-positive velocity is a script error — `None` here.
    pub fn new(
        id: ControllerId,
        user_arg: i32,
        target_yaw: f32,
        velocity: f32,
        hertz: f32,
    ) -> Option<Self> {
        if velocity <= 0.0 {
            return None;
        }
        Some(Self {
            id,
            user_arg,
            radians_per_tick: velocity / hertz,
            target_yaw: angle_normalise(target_yaw),
        })
    }

    /// `YawRotatorController::turn` (`turn_controller.cpp:107-133`):
    /// advance one tick's worth; snap and answer true when the step
    /// reached (or passed) the target.
    pub fn turn(&mut self, state: &mut EntityState) -> bool {
        let mut direction = state.direction;
        let current_yaw = angle_normalise(direction.yaw);
        let mut new_yaw = angle_normalise(direction.yaw + self.radians_per_tick);

        let reached_yaw = if self.radians_per_tick > 0.0 {
            angle_is_between(self.target_yaw, current_yaw, new_yaw)
        } else {
            angle_is_between(self.target_yaw, new_yaw, current_yaw)
        };

        if reached_yaw {
            new_yaw = self.target_yaw;
        }

        direction.yaw = new_yaw;
        state.direction = direction;
        reached_yaw
    }

    /// `YawRotatorController::startReal` (`turn_controller.cpp:143-160`):
    /// flip to the anti-clockwise direction when the target lies between
    /// `yaw - π` and `yaw`, then turn once unconditionally — the full
    /// circle face, where the target equals the current yaw.
    pub fn start(&mut self, state: &mut EntityState) -> bool {
        let yaw = angle_normalise(state.direction.yaw);
        if angle_is_between(self.target_yaw, yaw - MATH_PI, yaw) {
            self.radians_per_tick = -self.radians_per_tick;
        }
        self.turn(state)
    }

    /// `YawRotatorController::update` (`turn_controller.cpp:163-178`):
    /// arrive -> `onTurn` -> cancel. Answers true when cancelled.
    pub fn update(&mut self, state: &mut EntityState, log: &mut Vec<ScriptCallback>) -> bool {
        if !self.turn(state) {
            return false;
        }
        log.push(ScriptCallback {
            entity_id: state.entity_id,
            method: "onTurn",
            controller_id: self.id,
            user_arg: self.user_arg,
        });
        true
    }

    /// `writeRealToStream` (`turn_controller.cpp:181-186`).
    pub fn write_real_to_stream(&self, stream: &mut foundation::stream::BinaryStream) {
        stream.write_f32(self.target_yaw);
        stream.write_f32(self.radians_per_tick);
    }

    /// `readRealFromStream` (`turn_controller.cpp:189-196`).
    pub fn read_real_from_stream(
        &mut self,
        stream: &mut foundation::stream::BinaryStream,
    ) -> Result<(), foundation::stream::StreamError> {
        self.target_yaw = stream.read_f32()?;
        self.radians_per_tick = stream.read_f32()?;
        Ok(())
    }
}

// -----------------------------------------------------------------------------
// Section: BaseAccelerationController (`base_acceleration_controller.*`)
// -----------------------------------------------------------------------------

/// The `BaseAccelerationController` base (`base_acceleration_controller.hpp:47-79`):
/// steer the entity's velocity toward a destination by bounded
/// acceleration. The vehicle-dependent velocity cache
/// (`currVelocity_`'s vehicle branches) lands with BW-08-02-05 — without a
/// vehicle the reference just reads `pReal()->velocity()`.
#[derive(Clone, Debug)]
pub struct AccelerationCore {
    /// `acceleration_`, units/second².
    pub acceleration: f32,
    /// `maxSpeed_`, units/second.
    pub max_speed: f32,
    /// `facing_`.
    pub facing: u8,
    /// The update hertz this controller was built with.
    pub hertz: f32,
}

impl AccelerationCore {
    /// `BaseAccelerationController::BaseAccelerationController`
    /// (`base_acceleration_controller.cpp:113-124`).
    pub fn new(acceleration: f32, max_speed: f32, facing: u8, hertz: f32) -> Self {
        Self {
            acceleration,
            max_speed,
            facing,
            hertz,
        }
    }

    /// `calculateDesiredVelocity`
    /// (`base_acceleration_controller.cpp:315-349`): straight at the
    /// destination, braking at `sqrt(2 · d · a)` when the arrival must
    /// stop, capped at `maxSpeed`.
    fn calculate_desired_velocity(
        current_position: Vector3,
        desired_position: Vector3,
        acceleration: f32,
        max_speed: f32,
        stop_at_destination: bool,
    ) -> Vector3 {
        let destination_vector = desired_position - current_position;
        let mut desired_velocity = unit_vector(destination_vector);

        if stop_at_destination {
            // The speed an object accelerating from zero would reach over
            // the remaining distance.
            let speed_at_distance = (2.0 * destination_vector.length() * acceleration).sqrt();
            desired_velocity = desired_velocity * max_speed.min(speed_at_distance);
        } else {
            desired_velocity = desired_velocity * max_speed;
        }

        desired_velocity
    }

    /// `calculateAccelerationVector`
    /// (`base_acceleration_controller.cpp:360-403`): the direction to
    /// push the current velocity so it meets the desired one.
    fn calculate_acceleration_vector(
        current_velocity: Vector3,
        desired_velocity: Vector3,
    ) -> Vector3 {
        if vectors_almost_equal(current_velocity, desired_velocity) {
            return unit_vector(desired_velocity);
        }

        if almost_zero(desired_velocity.length_squared(), 0.0004) {
            return unit_vector(-current_velocity);
        }

        let mut acceleration_vector;
        if current_velocity.length_squared() > 0.0 {
            // (The reference also builds `unitCurrentVelocity` here; it is
            // a dead local and never feeds the projection.)
            let velocity_difference = desired_velocity - current_velocity;

            // projectOnto(v1, v2) = v2 * (v1·v2 / |v2|²)
            // (`vector3.ipp:184-188`).
            acceleration_vector = desired_velocity
                * (velocity_difference.dot_product(&desired_velocity)
                    / desired_velocity.length_squared());
            acceleration_vector =
                acceleration_vector + (velocity_difference - acceleration_vector) * 2.0;
        } else {
            acceleration_vector = desired_velocity;
        }

        unit_vector(acceleration_vector)
    }

    /// `BaseAccelerationController::move`
    /// (`base_acceleration_controller.cpp:175-272`). Without a vehicle the
    /// velocity is `pReal()->velocity()` (`refreshCurrentVelocity`,
    /// `base_acceleration_controller.cpp:126-160`).
    pub fn r#move(
        &mut self,
        state: &mut EntityState,
        destination: Vector3,
        stop_at_destination: bool,
        now: u32,
    ) -> bool {
        let current_velocity = state.velocity;

        let mut position = state.position;
        let mut direction = state.direction;

        let mut desired_velocity = Self::calculate_desired_velocity(
            position,
            destination,
            self.acceleration,
            self.max_speed,
            stop_at_destination,
        );

        if desired_velocity.length() > self.max_speed {
            desired_velocity.normalise();
            desired_velocity = desired_velocity * self.max_speed;
        }

        let acceleration_vector =
            Self::calculate_acceleration_vector(current_velocity, desired_velocity);

        let destination_vector = destination - position;
        let velocity_difference = current_velocity - desired_velocity;
        let acceleration_per_tick = self.acceleration / self.hertz;

        let new_velocity = if velocity_difference.length() > acceleration_per_tick {
            current_velocity + (acceleration_vector * acceleration_per_tick)
        } else {
            desired_velocity
        };

        // If we are real close, then finish up.
        let step_distance = new_velocity.length() / self.hertz;
        if step_distance > destination_vector.length() {
            position = destination;
        } else {
            position = position + (new_velocity / self.hertz);
        }

        // Make sure we are facing the right direction.
        if self.facing != FACING_NONE {
            let facing_vector =
                if self.facing == FACING_VELOCITY && new_velocity.length_squared() > 0.0 {
                    unit_vector(new_velocity)
                } else if self.facing == FACING_ACCELERATION {
                    // Avoid facing in odd angles when our current velocity is
                    // almost perfect.
                    if velocity_difference.length() > acceleration_per_tick {
                        acceleration_vector
                    } else {
                        unit_vector(new_velocity)
                    }
                } else {
                    Vector3::ZERO
                };

            if !is_zero(facing_vector.x) || !is_zero(facing_vector.z) {
                direction.yaw = facing_vector.yaw();
            }
            if !is_zero(facing_vector.y) {
                direction.pitch = facing_vector.pitch();
            }
        }

        // No longer on the ground...
        state.is_on_ground = false;
        state.set_position_and_direction(position, direction, now, self.hertz);

        if stop_at_destination {
            vectors_almost_equal(position, destination)
                && almost_zero(current_velocity.length_squared(), 0.0004)
        } else {
            vectors_almost_equal(position, destination)
        }
    }

    /// `writeRealToStream` (`base_acceleration_controller.cpp:280-286`).
    pub fn write_real_to_stream(&self, stream: &mut foundation::stream::BinaryStream) {
        stream.write_f32(self.acceleration);
        stream.write_f32(self.max_speed);
        stream.write_u8(self.facing);
        stream.write_f32(0.0);
        stream.write_f32(0.0);
        stream.write_f32(0.0);
    }

    /// `readRealFromStream` (`base_acceleration_controller.cpp:292-301`);
    /// the streamed `currVelocity_` cache is vehicle state and lands with
    /// BW-08-02-05, so it is consumed and dropped.
    pub fn read_real_from_stream(
        &mut self,
        stream: &mut foundation::stream::BinaryStream,
    ) -> Result<(), foundation::stream::StreamError> {
        self.acceleration = stream.read_f32()?;
        self.max_speed = stream.read_f32()?;
        self.facing = stream.read_u8()?;
        let _cached_velocity = Vector3::from_stream_bytes(stream)?;
        Ok(())
    }
}

/// `AccelerateToPointController` (`accelerate_to_point_controller.hpp:22-36`).
#[derive(Clone, Debug)]
pub struct AccelerateToPointController {
    /// The controller id.
    pub id: ControllerId,
    /// The script's user data.
    pub user_arg: i32,
    /// The `BaseAccelerationController` base.
    pub core: AccelerationCore,
    /// `destination_`.
    pub destination: Vector3,
    /// `stopAtDestination_`.
    pub stop_at_destination: bool,
}

impl AccelerateToPointController {
    /// The constructor (`accelerate_to_point_controller.cpp:38-58`).
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id: ControllerId,
        user_arg: i32,
        destination: Vector3,
        acceleration: f32,
        max_speed: f32,
        facing: u8,
        stop_at_destination: bool,
        hertz: f32,
    ) -> Self {
        Self {
            id,
            user_arg,
            core: AccelerationCore::new(acceleration, max_speed, facing, hertz),
            destination,
            stop_at_destination,
        }
    }

    /// `AccelerateToPointController::update`
    /// (`accelerate_to_point_controller.cpp:96-119`): arrive -> `onMove`
    /// -> cancel. Answers true when cancelled.
    pub fn update(
        &mut self,
        state: &mut EntityState,
        now: u32,
        log: &mut Vec<ScriptCallback>,
    ) -> bool {
        if !self
            .core
            .r#move(state, self.destination, self.stop_at_destination, now)
        {
            return false;
        }
        log.push(ScriptCallback {
            entity_id: state.entity_id,
            method: "onMove",
            controller_id: self.id,
            user_arg: self.user_arg,
        });
        true
    }

    /// `writeRealToStream` (`accelerate_to_point_controller.cpp:61-69`).
    pub fn write_real_to_stream(&self, stream: &mut foundation::stream::BinaryStream) {
        self.core.write_real_to_stream(stream);
        stream.write_f32(self.destination.x);
        stream.write_f32(self.destination.y);
        stream.write_f32(self.destination.z);
        stream.write_bool(self.stop_at_destination);
    }

    /// `readRealFromStream` (`accelerate_to_point_controller.cpp:72-81`).
    pub fn read_real_from_stream(
        &mut self,
        stream: &mut foundation::stream::BinaryStream,
    ) -> Result<(), foundation::stream::StreamError> {
        self.core.read_real_from_stream(stream)?;
        self.destination = Vector3::from_stream_bytes(stream)?;
        self.stop_at_destination = stream.read_bool()?;
        Ok(())
    }
}

/// `AccelerateAlongPathController` (`accelerate_along_path_controller.hpp:20-35`):
/// walk a waypoint list; only the last leg stops at its end.
#[derive(Clone, Debug)]
pub struct AccelerateAlongPathController {
    /// The controller id.
    pub id: ControllerId,
    /// The script's user data.
    pub user_arg: i32,
    /// The `BaseAccelerationController` base.
    pub core: AccelerationCore,
    /// `waypoints_` (local space).
    pub waypoints: Vec<Vector3>,
    /// `progress_` — the index of the current waypoint.
    pub progress: usize,
}

impl AccelerateAlongPathController {
    /// The waypoint constructor
    /// (`accelerate_along_path_controller.cpp:47-64`). The reference
    /// asserts a non-empty list.
    pub fn new(
        id: ControllerId,
        user_arg: i32,
        waypoints: Vec<Vector3>,
        acceleration: f32,
        max_speed: f32,
        facing: u8,
        hertz: f32,
    ) -> Self {
        debug_assert!(!waypoints.is_empty(), "MF_ASSERT( !waypoints_.empty() )");
        Self {
            id,
            user_arg,
            core: AccelerationCore::new(acceleration, max_speed, facing, hertz),
            waypoints,
            progress: 0,
        }
    }

    /// `AccelerateAlongPathController::update`
    /// (`accelerate_along_path_controller.cpp:97-135`): intermediate
    /// waypoints cruise through (`stop=false`, no callback); the last one
    /// stops, fires `onMove` and cancels. Answers true when cancelled.
    pub fn update(
        &mut self,
        state: &mut EntityState,
        now: u32,
        log: &mut Vec<ScriptCallback>,
    ) -> bool {
        let is_last_waypoint = self.progress == self.waypoints.len() - 1;
        let waypoint = self.waypoints[self.progress];

        if !self.core.r#move(state, waypoint, is_last_waypoint, now) {
            return false;
        }

        self.progress += 1;

        if is_last_waypoint {
            log.push(ScriptCallback {
                entity_id: state.entity_id,
                method: "onMove",
                controller_id: self.id,
                user_arg: self.user_arg,
            });
            return true;
        }
        false
    }

    /// `writeRealToStream`
    /// (`accelerate_along_path_controller.cpp:67-75`).
    pub fn write_real_to_stream(&self, stream: &mut foundation::stream::BinaryStream) {
        self.core.write_real_to_stream(stream);
        stream.write_u32(self.waypoints.len() as u32);
        for waypoint in &self.waypoints {
            stream.write_f32(waypoint.x);
            stream.write_f32(waypoint.y);
            stream.write_f32(waypoint.z);
        }
        stream.write_u32(self.progress as u32);
    }

    /// `readRealFromStream`
    /// (`accelerate_along_path_controller.cpp:78-89`).
    pub fn read_real_from_stream(
        &mut self,
        stream: &mut foundation::stream::BinaryStream,
    ) -> Result<(), foundation::stream::StreamError> {
        self.core.read_real_from_stream(stream)?;
        let count = stream.read_u32()? as usize;
        self.waypoints.clear();
        for _ in 0..count {
            self.waypoints.push(Vector3::from_stream_bytes(stream)?);
        }
        self.progress = stream.read_u32()? as usize;
        Ok(())
    }
}

/// `AccelerateToEntityController` (`accelerate_to_entity_controller.hpp:23-42`):
/// the accelerating variant of `MoveToEntityController`.
#[derive(Clone, Debug)]
pub struct AccelerateToEntityController {
    /// The controller id.
    pub id: ControllerId,
    /// The script's user data.
    pub user_arg: i32,
    /// The `BaseAccelerationController` base.
    pub core: AccelerationCore,
    /// `destEntityID_`.
    pub dest_entity_id: EntityId,
    /// `range_`.
    pub range: f32,
    /// `offset_`, a random point within range of the destination.
    pub offset: Vector3,
    /// Whether the destination entity has been resolved.
    resolved: bool,
}

impl AccelerateToEntityController {
    /// The constructor (`accelerate_to_entity_controller.cpp:31-48`).
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id: ControllerId,
        user_arg: i32,
        dest_entity_id: EntityId,
        acceleration: f32,
        max_speed: f32,
        facing: u8,
        range: f32,
        hertz: f32,
    ) -> Self {
        Self {
            id,
            user_arg,
            core: AccelerationCore::new(acceleration, max_speed, facing, hertz),
            dest_entity_id,
            range,
            offset: Vector3::ZERO,
            resolved: false,
        }
    }

    /// `AccelerateToEntityController::recalcOffset`
    /// (`accelerate_to_entity_controller.cpp:84-90`).
    fn recalc_offset(&mut self, rng: u64) {
        let angle = (rng % 360) as f32 * (MATH_PI / 180.0);
        self.offset.x = self.range * angle.cos();
        self.offset.z = self.range * angle.sin();
    }

    /// `AccelerateToEntityController::update`
    /// (`accelerate_to_entity_controller.cpp:98-199`): always
    /// stop-at-destination; inside range it faces the target exactly
    /// (when facing is enabled) and fires `onMove`; a missing target
    /// fires `onMoveFailure`. Answers true when cancelled.
    pub fn update(
        &mut self,
        state: &mut EntityState,
        target: Option<Vector3>,
        rng: u64,
        now: u32,
        log: &mut Vec<ScriptCallback>,
    ) -> bool {
        if !self.resolved {
            self.resolved = true;
            self.recalc_offset(rng);
        }

        let Some(destination) = target else {
            log.push(ScriptCallback {
                entity_id: state.entity_id,
                method: "onMoveFailure",
                controller_id: self.id,
                user_arg: self.user_arg,
            });
            return true;
        };

        let remaining = destination - state.position;
        let close_enough = remaining.length() < self.range;

        let dest_local = destination + self.offset;

        if close_enough || self.core.r#move(state, dest_local, true, now) {
            if close_enough {
                // In case we were moving towards a point near the entity,
                // make sure that we are now facing it exactly.
                let vector = destination - state.position;
                if self.core.facing != FACING_NONE {
                    state.direction.yaw = vector.yaw();
                }
            }

            log.push(ScriptCallback {
                entity_id: state.entity_id,
                method: "onMove",
                controller_id: self.id,
                user_arg: self.user_arg,
            });
            return true;
        }
        false
    }

    /// `writeRealToStream`
    /// (`accelerate_to_entity_controller.cpp:51-58`).
    pub fn write_real_to_stream(&self, stream: &mut foundation::stream::BinaryStream) {
        self.core.write_real_to_stream(stream);
        stream.write_u32(self.dest_entity_id);
        stream.write_f32(self.range);
    }

    /// `readRealFromStream`
    /// (`accelerate_to_entity_controller.cpp:61-70`).
    pub fn read_real_from_stream(
        &mut self,
        stream: &mut foundation::stream::BinaryStream,
    ) -> Result<(), foundation::stream::StreamError> {
        self.core.read_real_from_stream(stream)?;
        self.dest_entity_id = stream.read_u32()?;
        self.range = stream.read_f32()?;
        self.resolved = false;
        Ok(())
    }
}

// -----------------------------------------------------------------------------
// Section: Tests
// -----------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use foundation::stream::BinaryStream;

    const HERTZ: f32 = DEFAULT_GAME_UPDATE_HERTZ;

    fn state_at(position: Vector3) -> EntityState {
        EntityState::new(7, position, Direction3D::default(), 0)
    }

    fn callback_methods(log: &[ScriptCallback]) -> Vec<&'static str> {
        log.iter().map(|c| c.method).collect()
    }

    fn approx(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-4
    }

    fn approx_v(a: Vector3, b: Vector3) -> bool {
        approx(a.x, b.x) && approx(a.y, b.y) && approx(a.z, b.z)
    }

    // -------------------------------------------------------------------
    // TDD: BW-08-02-03 移植移动、转向、加速控制 — 固定输入轨迹吻合
    // -------------------------------------------------------------------

    /// The scalar faces: exact `isZero`, the 0.0004 epsilons, and the
    /// `Angle` wrap/range semantics the controllers lean on.
    #[test]
    fn scalar_and_angle_helpers_match_the_reference() {
        assert!(is_zero(0.0));
        assert!(!is_zero(1e-30), "isEqual is fabsf(diff) <= 0: exact only");
        assert!(almost_equal(1.0, 1.0 + 0.00039));
        assert!(!almost_equal(1.0, 1.0 + 0.00041));
        assert!(almost_zero(0.00039, 0.0004));
        assert!(!almost_zero(-0.00041, 0.0004));

        // Angle::normalise wraps into (-π, π].
        assert_eq!(angle_normalise(0.0), 0.0);
        assert!(approx(angle_normalise(MATH_PI * 3.0), MATH_PI));
        // -3π: floor((-3π+π)/2π)·2π = -2π, so the result is -π.
        assert!(approx(angle_normalise(-MATH_PI * 3.0), -MATH_PI));
        // Past ±3π the floor branch peels off the full turn count.
        assert!(approx(angle_normalise(MATH_PI * 4.0 + 0.25), 0.25));
        assert!(approx(angle_normalise(-MATH_PI * 4.0 - 0.25), -0.25));
        // -2.5π sits in (-3π, -π]: the plain +2π branch.
        assert!(approx(angle_normalise(-MATH_PI * 2.5), -MATH_PI / 2.0));
        assert!(approx(angle_normalise(MATH_PI * 2.5), MATH_PI / 2.0));
        assert!(approx(angle_normalise(-MATH_PI * 0.5), -MATH_PI / 2.0));

        // turnRange measures clockwise; isBetween is `diff <= range`.
        assert!(approx(turn_range(0.0, MATH_PI / 2.0), MATH_PI / 2.0));
        assert!(approx(turn_range(MATH_PI / 2.0, 0.0), MATH_PI * 1.5));
        assert!(angle_is_between(
            angle_normalise(MATH_PI / 4.0),
            0.0,
            MATH_PI / 2.0
        ));
        assert!(!angle_is_between(
            angle_normalise(MATH_PI),
            0.0,
            MATH_PI / 2.0
        ));
        // The inclusive boundary is what makes a full-circle rotator finish
        // on its first tick.
        assert!(angle_is_between(1.0, 1.0, 1.0));
    }

    /// `MoveToPointController` walks a fixed straight-line trajectory: 0.3
    /// units per tick at velocity 3 / hertz 10, snapping on the tick the
    /// remaining distance drops inside the step, facing the movement, and
    /// firing `onMove` once before cancelling.
    #[test]
    fn move_to_point_walks_a_fixed_trajectory() {
        let mut controller =
            MoveToPointController::new(4, 9, Vector3::new(10.0, 0.0, 0.0), 3.0, true, false, HERTZ);
        assert!(approx(controller.core.metres_per_tick, 0.3));
        let mut state = state_at(Vector3::ZERO);
        let mut log = Vec::new();

        let mut ticks = 0;
        while !controller.update(&mut state, ticks + 1, &mut log) {
            ticks += 1;
            assert!(ticks < 100, "must arrive");
        }
        ticks += 1;

        // 33 ticks of 0.3 = 9.9; tick 34 snaps the last 0.1.
        assert_eq!(ticks, 34);
        assert!(approx_v(state.position, Vector3::new(10.0, 0.0, 0.0)));
        // Facing the movement: yaw = atan2(10, 0) = π/2, pitch 0.
        assert!(approx(state.direction.yaw, MATH_PI / 2.0));
        assert_eq!(state.direction.pitch, 0.0);
        assert!(!state.is_on_ground);
        assert_eq!(
            log,
            vec![ScriptCallback {
                entity_id: 7,
                method: "onMove",
                controller_id: 4,
                user_arg: 9,
            }]
        );
    }

    /// The movement flags: `faceMovement=false` keeps the original
    /// direction, `moveVertically=false` walks flat, and a diagonal move
    /// with vertical motion faces with both yaw and pitch.
    #[test]
    fn movement_flags_shape_the_trajectory() {
        // faceMovement=false: yaw untouched (velocity 10 -> 1.0 per tick,
        // 10 ticks to arrive).
        let mut controller = MoveToPointController::new(
            1,
            0,
            Vector3::new(10.0, 0.0, 0.0),
            10.0,
            false,
            false,
            HERTZ,
        );
        let mut state = state_at(Vector3::ZERO);
        state.direction = Direction3D::new(0.25, 0.5, 0.0);
        let mut log = Vec::new();
        let mut now = 1u32;
        while !controller.update(&mut state, now, &mut log) {
            now += 1;
            assert!(now < 100);
        }
        assert!(approx(state.direction.yaw, 0.25));
        assert!(approx(state.direction.pitch, 0.5));
        assert_eq!(callback_methods(&log), vec!["onMove"]);

        // moveVertically=false: a raised destination is walked at ground
        // height and the arrival is satisfied flat.
        let mut controller =
            MoveToPointController::new(1, 0, Vector3::new(5.0, 3.0, 0.0), 5.0, true, false, HERTZ);
        let mut state = state_at(Vector3::new(0.0, 1.0, 0.0));
        let mut log = Vec::new();
        while !controller.update(&mut state, 1, &mut log) {
            assert_eq!(state.position.y, 1.0, "y is frozen");
        }
        assert!(approx_v(state.position, Vector3::new(5.0, 1.0, 0.0)));

        // Vertical motion faces pitch on the y component.
        let mut controller =
            MoveToPointController::new(1, 0, Vector3::new(0.0, 5.0, 0.0), 5.0, true, true, HERTZ);
        let mut state = state_at(Vector3::ZERO);
        let mut log = Vec::new();
        while !controller.update(&mut state, 1, &mut log) {}
        assert!(
            state.direction.pitch < 0.0,
            "rising faces up (negative pitch)"
        );
    }

    /// The navigation-drop branch (`shouldNavigationDropPosition`): the
    /// position is lifted by climb + slope · step so the navigator's
    /// ground snap has headroom.
    #[test]
    fn navigation_drop_lifts_the_position() {
        let mut controller =
            MoveToPointController::new(1, 0, Vector3::new(4.0, 0.0, 0.0), 4.0, false, false, HERTZ);
        controller.core.navigation_drop_position = true;
        let mut state = state_at(Vector3::ZERO);
        let mut log = Vec::new();
        controller.update(&mut state, 1, &mut log);

        // One step of 0.4: y = 0.65 + tan(45°) * 0.4.
        assert!(approx(state.position.y, 0.65 + 1.0 * 0.4));
    }

    /// `MoveToEntityController` stops inside the range, faces the target
    /// exactly, and fails cleanly when the target vanishes; the fixed rng
    /// pins the within-range offset.
    #[test]
    fn move_to_entity_stops_in_range_and_fails_cleanly() {
        let mut controller = MoveToEntityController::new(2, 5, 8, 4.0, 2.0, true, false, HERTZ);
        let mut state = state_at(Vector3::ZERO);
        let mut log = Vec::new();

        // rng = 90: the offset lands due south of the target.
        let target = Vector3::new(6.0, 0.0, 0.0);
        let mut ticks = 0;
        while !controller.update(&mut state, Some(target), 90, ticks + 1, &mut log) {
            ticks += 1;
            assert!(ticks < 100);
        }
        // The entity ended within range of the (y-adjusted, offset)
        // destination and faces the target exactly.
        assert!(approx(state.direction.yaw, (target - state.position).yaw()));
        assert_eq!(
            callback_methods(&log),
            vec!["onMove"],
            "the offset approach still finishes with one onMove"
        );
        let distance = (target - state.position).length();
        assert!(distance <= 2.0 + controller.core.metres_per_tick + 0.01);

        // A vanished target fails with onMoveFailure and cancels.
        log.clear();
        let mut controller2 = MoveToEntityController::new(3, 0, 9, 4.0, 2.0, true, false, HERTZ);
        let mut state2 = state_at(Vector3::ZERO);
        assert!(
            controller2.update(&mut state2, None, 0, 1, &mut log),
            "missing target cancels immediately"
        );
        assert_eq!(callback_methods(&log), vec!["onMoveFailure"]);
    }

    /// `NavigateStepController` chains waypoints through `onMove`, spends
    /// the leftover tick fraction on the next leg, and cancels when the
    /// script navigates nowhere new.
    #[test]
    fn navigate_step_chains_waypoints_and_budgets_ticks() {
        let mut controller =
            NavigateStepController::new(6, 0, Vector3::new(0.2, 0.0, 0.0), 3.0, true, HERTZ);
        assert!(approx(controller.core.metres_per_tick, 0.3));
        let mut state = state_at(Vector3::ZERO);
        let mut log = Vec::new();

        // Tick 1: the 0.2 leg arrives with 1/3 of a tick left and fires
        // onMove; the script sets the next waypoint at 0.45, and the
        // leftover budget (0.1 units of it) walks to 0.3 before running
        // out — no second callback this tick.
        let mut next = Some(Vector3::new(0.45, 0.0, 0.0));
        let finished = controller.update(&mut state, 1, &mut || next.take(), &mut log);
        assert!(!finished);
        assert_eq!(callback_methods(&log), vec!["onMove"]);
        assert!(approx_v(state.position, Vector3::new(0.3, 0.0, 0.0)));
        assert!(approx(controller.destination.x, 0.45));

        // Tick 2: fresh budget; the 0.15 leg arrives with half a tick
        // left, onMove fires, the script navigates nowhere new, and the
        // controller cancels itself.
        let finished = controller.update(&mut state, 2, &mut || None, &mut log);
        assert!(finished);
        assert_eq!(callback_methods(&log), vec!["onMove", "onMove"]);
        assert!(approx_v(state.position, Vector3::new(0.45, 0.0, 0.0)));

        // The script-facing destination setter (`move_controller.cpp:349-352`).
        controller.set_destination(Vector3::new(9.0, 0.0, 0.0));
        assert_eq!(controller.destination, Vector3::new(9.0, 0.0, 0.0));

        // The 100-loop guard: a script that keeps re-navigating to the
        // point it already stands on consumes no budget (the leftover
        // fraction is exactly 1), so the loop runs away until
        // onMoveFailure and the cancel.
        let mut controller = NavigateStepController::new(7, 0, Vector3::ZERO, 3.0, true, HERTZ);
        let mut state = state_at(Vector3::ZERO);
        let mut log = Vec::new();
        let finished = controller.update(&mut state, 1, &mut || Some(Vector3::ZERO), &mut log);
        assert!(finished);
        let methods = callback_methods(&log);
        assert_eq!(methods.len(), 100);
        assert_eq!(&methods[..99], &["onMove"; 99][..]);
        assert_eq!(methods[99], "onMoveFailure");
        assert!(approx_v(state.position, Vector3::ZERO));
    }

    /// `YawRotatorController` picks the shorter way round, walks a fixed
    /// angular trajectory, and fires `onTurn` with the controller id and
    /// user arg on arrival; a non-positive velocity is rejected.
    #[test]
    fn yaw_rotator_turns_the_shortest_path_and_fires_on_turn() {
        // Facing 0, target 3π/2 (normalised to -π/2): anti-clockwise is
        // shorter, so the start flips the positive rate negative.
        let mut controller = YawRotatorController::new(3, 8, MATH_PI * 1.5, 1.0, HERTZ).unwrap();
        assert!(approx(controller.target_yaw, -MATH_PI / 2.0));
        assert!(approx(controller.radians_per_tick, 0.1));

        let mut state = state_at(Vector3::ZERO);
        let mut log = Vec::new();
        let start = controller.start(&mut state);
        assert!(approx(controller.radians_per_tick, -0.1), "sign flipped");
        assert!(!start, "the unconditional first turn is not the target");
        assert!(approx(state.direction.yaw, -0.1));

        let mut ticks = 1;
        while !controller.update(&mut state, &mut log) {
            ticks += 1;
            assert!(ticks < 100);
        }
        // 1.5708/0.1 = ~16 ticks; the exact arrival tick rides on the f32
        // accumulation of -0.1, so accept 15..=17 — the snap then lands
        // the yaw exactly on the target either way.
        assert!((15..=17).contains(&ticks), "ticks = {ticks}");
        assert_eq!(state.direction.yaw, controller.target_yaw);
        assert!(approx(state.direction.yaw, -MATH_PI / 2.0));
        assert_eq!(
            log,
            vec![ScriptCallback {
                entity_id: 7,
                method: "onTurn",
                controller_id: 3,
                user_arg: 8,
            }]
        );

        // A non-positive velocity is the script error face.
        assert!(YawRotatorController::new(1, 0, 0.0, 0.0, HERTZ).is_none());
        assert!(YawRotatorController::new(1, 0, 0.0, -1.0, HERTZ).is_none());
    }

    /// The full-circle face: the target equals the current yaw, the start
    /// flips the direction (the target is on the inclusive arc behind),
    /// and the very next update lands on the target.
    #[test]
    fn yaw_rotator_handles_the_same_yaw_target() {
        let mut controller = YawRotatorController::new(1, 0, 0.5, 1.0, HERTZ).unwrap();
        let mut state = state_at(Vector3::ZERO);
        state.direction.yaw = 0.5;
        let mut log = Vec::new();

        // start flips to negative and its unconditional turn already snaps
        // to the target (the arc is inclusive).
        let _ = controller.start(&mut state);
        assert!(
            controller.update(&mut state, &mut log),
            "finishes immediately"
        );
        assert_eq!(callback_methods(&log), vec!["onTurn"]);
    }

    /// The clockwise face: a target ahead within π keeps the positive
    /// rate, the rotator walks forward, and snaps onto the target with a
    /// single `onTurn`.
    #[test]
    fn yaw_rotator_turns_clockwise_when_the_target_is_ahead() {
        let mut controller = YawRotatorController::new(1, 0, 1.0, 1.0, HERTZ).unwrap();
        let mut state = state_at(Vector3::ZERO);
        let mut log = Vec::new();

        // 1.0 is ahead of 0 (not on the yaw-π..yaw arc): no flip, and the
        // unconditional first turn advances one step.
        assert!(!controller.start(&mut state));
        assert!(approx(controller.radians_per_tick, 0.1), "rate untouched");
        assert!(approx(state.direction.yaw, 0.1));

        let mut ticks = 1;
        while !controller.update(&mut state, &mut log) {
            ticks += 1;
            assert!(ticks < 100);
        }
        assert_eq!(state.direction.yaw, controller.target_yaw);
        assert!(approx(state.direction.yaw, 1.0));
        assert_eq!(callback_methods(&log), vec!["onTurn"]);
    }

    /// The acceleration-facing branches: `FACING_ACCELERATION` faces the
    /// acceleration vector while the velocity difference still exceeds
    /// the per-tick budget and falls back to the new velocity once it
    /// matches; a vertical facing component drives the pitch.
    #[test]
    fn acceleration_facing_covers_the_acceleration_and_pitch_branches() {
        let mut log = Vec::new();
        let mut controller = AccelerateToPointController::new(
            1,
            0,
            Vector3::new(4.0, 0.0, 0.0),
            2.0,
            2.0,
            FACING_ACCELERATION,
            false,
            HERTZ,
        );
        let mut state = state_at(Vector3::ZERO);

        // From rest: the acceleration vector points at the destination.
        controller.update(&mut state, 1, &mut log);
        assert!(
            approx(state.direction.yaw, MATH_PI / 2.0),
            "faced the acceleration"
        );

        // Once the velocity matches the desired, the fallback faces the
        // new velocity instead.
        state.velocity = Vector3::new(2.0, 0.0, 0.0);
        controller.update(&mut state, 2, &mut log);
        assert!(
            approx(state.direction.yaw, MATH_PI / 2.0),
            "faced the velocity fallback"
        );

        // A vertical facing component drives the pitch (rising faces up).
        let mut controller = AccelerateToPointController::new(
            2,
            0,
            Vector3::new(0.0, 8.0, 0.0),
            2.0,
            2.0,
            FACING_VELOCITY,
            false,
            HERTZ,
        );
        let mut state = state_at(Vector3::ZERO);
        controller.update(&mut state, 1, &mut log);
        assert!(
            state.direction.pitch < 0.0,
            "rising faces up: {}",
            state.direction.pitch
        );
    }

    /// `BaseAccelerationController`'s static faces: the braking
    /// `sqrt(2·d·a)` desired speed with its `maxSpeed` cap, and every
    /// branch of the acceleration direction.
    #[test]
    fn desired_velocity_and_acceleration_vector_match_the_reference() {
        // Cruise: maxSpeed along the unit direction.
        let desired = AccelerationCore::calculate_desired_velocity(
            Vector3::ZERO,
            Vector3::new(10.0, 0.0, 0.0),
            2.0,
            3.0,
            false,
        );
        assert!(approx_v(desired, Vector3::new(3.0, 0.0, 0.0)));

        // Stop: sqrt(2·d·a) = sqrt(2·1·2) = 2 under the 3 cap.
        let desired = AccelerationCore::calculate_desired_velocity(
            Vector3::ZERO,
            Vector3::new(1.0, 0.0, 0.0),
            2.0,
            3.0,
            true,
        );
        assert!(approx_v(desired, Vector3::new(2.0, 0.0, 0.0)));

        // Stop close: sqrt(2·0.1·2) ≈ 0.632 wins under the cap... no: it
        // scales the unit vector, so it is capped at 0.632 < 3.
        let desired = AccelerationCore::calculate_desired_velocity(
            Vector3::ZERO,
            Vector3::new(0.1, 0.0, 0.0),
            2.0,
            3.0,
            true,
        );
        assert!(approx(desired.length(), (2.0 * 0.1 * 2.0f32).sqrt()));

        // Acceleration direction: velocities equal -> the desired unit.
        let acc = AccelerationCore::calculate_acceleration_vector(
            Vector3::new(2.0, 0.0, 0.0),
            Vector3::new(2.0, 0.0, 0.0),
        );
        assert!(approx_v(acc, Vector3::new(1.0, 0.0, 0.0)));

        // Desired ~zero -> decelerate straight back.
        let acc = AccelerationCore::calculate_acceleration_vector(
            Vector3::new(2.0, 0.0, 0.0),
            Vector3::ZERO,
        );
        assert!(approx_v(acc, Vector3::new(-1.0, 0.0, 0.0)));

        // From rest -> straight at the desired velocity.
        let acc = AccelerationCore::calculate_acceleration_vector(
            Vector3::ZERO,
            Vector3::new(0.0, 0.0, 5.0),
        );
        assert!(approx_v(acc, Vector3::new(0.0, 0.0, 1.0)));

        // In motion -> the projection blend of the velocity difference.
        let acc = AccelerationCore::calculate_acceleration_vector(
            Vector3::new(3.0, 0.0, 0.0),
            Vector3::new(0.0, 3.0, 0.0),
        );
        let velocity_difference = Vector3::new(-3.0, 3.0, 0.0);
        let projected = Vector3::new(0.0, 3.0, 0.0)
            * (velocity_difference.dot_product(&Vector3::new(0.0, 3.0, 0.0))
                / Vector3::new(0.0, 3.0, 0.0).length_squared());
        let expected = projected + (velocity_difference - projected) * 2.0;
        assert!(approx_v(acc, unit_vector(expected)));
    }

    /// `AccelerateToPointController` from rest: the sample feedback loop
    /// (`RealEntity::newPosition`'s `dur > 1` guard resamples every other
    /// tick) lets the acceleration actually accumulate — the speed ramps
    /// 0.2 per sample pair up to the maxSpeed cap, cruises, brakes into
    /// the destination, and settles with a single `onMove`.
    #[test]
    fn accelerate_to_point_ramps_caps_brakes_and_arrives() {
        let mut controller = AccelerateToPointController::new(
            5,
            3,
            Vector3::new(5.0, 0.0, 0.0),
            2.0,
            2.0,
            FACING_VELOCITY,
            true,
            HERTZ,
        );
        let mut state = state_at(Vector3::ZERO);
        let mut log = Vec::new();

        let mut ticks = 0;
        loop {
            ticks += 1;
            if controller.update(&mut state, ticks, &mut log) {
                break;
            }
            assert!(
                ticks < 400,
                "must arrive: pos {:?} speed {:?}",
                state.position,
                state.velocity
            );
            if ticks == 4 {
                // The ramp: 0.02, 0.04, 0.06 steps land at 0.12.
                assert!(
                    approx_v(state.position, Vector3::new(0.12, 0.0, 0.0)),
                    "pos {:?}",
                    state.position
                );
            }
            if ticks == 10 {
                // Sampled speed has reached 1.0.
                assert!(approx(state.velocity.x, 1.0), "vel {:?}", state.velocity);
            }
            if ticks == 20 {
                // Capped at maxSpeed and cruising at 0.2 per tick.
                assert!(approx(state.velocity.x, 2.0), "vel {:?}", state.velocity);
                assert!(
                    approx_v(state.position, Vector3::new(2.2, 0.0, 0.0)),
                    "pos {:?}",
                    state.position
                );
            }
        }

        // Arrives around tick 39: snapped onto the destination, the
        // sampled velocity has decayed to zero (the settle test), and the
        // facing is the motion (+x is yaw π/2).
        assert!(vectors_almost_equal(
            state.position,
            Vector3::new(5.0, 0.0, 0.0)
        ));
        assert_eq!(state.velocity, Vector3::ZERO);
        assert!(approx(state.direction.yaw, MATH_PI / 2.0), "faced +x");
        assert_eq!(callback_methods(&log), vec!["onMove"]);
    }

    /// A seed velocity rides the same feedback loop: the cruise holds the
    /// 0.2-per-tick maxSpeed step until the braking curve bites at d < 1,
    /// the position steps shrink, the snap lands on the destination, and
    /// the sample of the stopped position decays the velocity to zero so
    /// the settle test passes with a single `onMove`.
    #[test]
    fn accelerate_to_point_brakes_a_seed_velocity_and_settles() {
        let mut controller = AccelerateToPointController::new(
            5,
            3,
            Vector3::new(5.0, 0.0, 0.0),
            2.0,
            2.0,
            FACING_VELOCITY,
            true,
            HERTZ,
        );
        let mut state = state_at(Vector3::ZERO);
        state.velocity = Vector3::new(2.0, 0.0, 0.0);
        let mut log = Vec::new();

        let mut ticks = 0;
        loop {
            ticks += 1;
            if controller.update(&mut state, ticks, &mut log) {
                break;
            }
            assert!(
                ticks < 400,
                "must arrive: pos {:?} speed {:?}",
                state.position,
                state.velocity
            );
            if ticks <= 20 {
                // Desired stays 2 while d >= 1 and the sampled velocity
                // confirms it: a clean 0.2 per tick cruise.
                assert!(
                    approx_v(state.position, Vector3::new(0.2 * ticks as f32, 0.0, 0.0)),
                    "cruise checkpoint at tick {ticks}: {:?}",
                    state.position
                );
            }
            if ticks > 20 {
                // Past d < 1 the position is ahead of the pure cruise
                // line... or on it; either way it never overshoots the
                // destination.
                assert!(
                    state.position.x <= 5.0 + 1e-3,
                    "overshoot at tick {ticks}: {}",
                    state.position.x
                );
            }
        }

        assert!(vectors_almost_equal(
            state.position,
            Vector3::new(5.0, 0.0, 0.0)
        ));
        assert_eq!(state.velocity, Vector3::ZERO);
        assert!(ticks <= 40, "settled at tick {ticks}");
        assert_eq!(callback_methods(&log), vec!["onMove"]);
    }

    /// `AccelerateAlongPathController` cruises through intermediate
    /// waypoints without stopping or callbacks and finishes on the last.
    #[test]
    fn accelerate_along_path_walks_waypoints_in_order() {
        let mut controller = AccelerateAlongPathController::new(
            2,
            0,
            vec![Vector3::new(2.0, 0.0, 0.0), Vector3::new(2.0, 0.0, 2.0)],
            4.0,
            2.0,
            FACING_NONE,
            HERTZ,
        );
        assert_eq!(controller.progress, 0);
        let mut state = state_at(Vector3::ZERO);
        let mut log = Vec::new();

        let mut ticks = 0;
        while !controller.update(&mut state, ticks + 1, &mut log) {
            ticks += 1;
            assert!(ticks < 400);
            if controller.progress == 1 {
                // The first waypoint passed without a callback.
                assert!(log.is_empty());
            }
        }

        assert_eq!(controller.progress, 2);
        assert!(vectors_almost_equal(
            state.position,
            Vector3::new(2.0, 0.0, 2.0)
        ));
        assert_eq!(callback_methods(&log), vec!["onMove"]);
        assert!(state.direction.yaw == 0.0, "FACING_NONE left yaw alone");
    }

    /// `AccelerateToEntityController` catches a slower target (running on
    /// the frozen seed velocity), stops inside the range facing it
    /// exactly, and fails cleanly when the target vanishes.
    #[test]
    fn accelerate_to_entity_chases_or_fails() {
        let mut controller =
            AccelerateToEntityController::new(4, 0, 9, 6.0, 3.0, FACING_VELOCITY, 1.0, HERTZ);
        let mut state = state_at(Vector3::ZERO);
        state.velocity = Vector3::new(2.0, 0.0, 0.0);
        let mut log = Vec::new();

        // The target drifts along +x at 0.1 units/tick from (4,0,0);
        // we close at ~0.2-0.26 per tick on the frozen seed.
        let mut ticks = 0;
        loop {
            let target = Vector3::new(4.0 + 0.1 * ticks as f32, 0.0, 0.0);
            if controller.update(&mut state, Some(target), 0, ticks + 1, &mut log) {
                break;
            }
            ticks += 1;
            assert!(ticks < 400, "must catch up: pos {:?}", state.position);
        }

        // Caught: within range of the target and facing it exactly.
        let target = Vector3::new(4.0 + 0.1 * ticks as f32, 0.0, 0.0);
        let gap = (target - state.position).length();
        assert!(gap < 1.0 + 0.3, "caught at gap {gap}");
        assert_eq!(callback_methods(&log), vec!["onMove"]);
        assert!(
            approx(state.direction.yaw, (target - state.position).yaw()),
            "faced the target exactly"
        );

        // A vanished target fails and cancels.
        log.clear();
        let mut controller2 =
            AccelerateToEntityController::new(5, 0, 10, 6.0, 3.0, FACING_NONE, 1.0, HERTZ);
        let mut state2 = state_at(Vector3::ZERO);
        assert!(controller2.update(&mut state2, None, 0, 1, &mut log));
        assert_eq!(callback_methods(&log), vec!["onMoveFailure"]);
    }

    /// The offload roundtrips: each controller's `writeRealToStream` ->
    /// `readRealFromStream` carries the trajectory state unchanged.
    #[test]
    fn controller_states_stream_roundtrip() {
        let mut move_point =
            MoveToPointController::new(1, 2, Vector3::new(1.0, 2.0, 3.0), 3.0, false, true, HERTZ);
        move_point.core.navigation_drop_position = true;
        let mut stream = BinaryStream::new();
        move_point.write_real_to_stream(&mut stream);
        stream.rewind();
        let mut restored = MoveToPointController::new(0, 0, Vector3::ZERO, 0.0, true, false, HERTZ);
        restored.read_real_from_stream(&mut stream).unwrap();
        assert!(stream.at_end());
        assert_eq!(restored.destination, move_point.destination);
        assert!(approx(
            restored.core.metres_per_tick,
            move_point.core.metres_per_tick
        ));
        assert!(!restored.core.face_movement);
        assert!(restored.core.move_vertically);

        let mut navigate_step =
            NavigateStepController::new(8, 4, Vector3::new(0.45, 0.0, 0.0), 3.0, true, HERTZ);
        navigate_step.core.navigation_drop_position = true;
        let mut stream = BinaryStream::new();
        navigate_step.write_real_to_stream(&mut stream);
        stream.rewind();
        let mut restored = NavigateStepController::new(0, 0, Vector3::ZERO, 0.0, true, HERTZ);
        restored.read_real_from_stream(&mut stream).unwrap();
        assert!(stream.at_end());
        assert_eq!(restored.destination, navigate_step.destination);
        assert!(approx(
            restored.core.metres_per_tick,
            navigate_step.core.metres_per_tick
        ));
        assert!(restored.core.face_movement);
        assert!(!restored.core.move_vertically);

        let yaw = YawRotatorController::new(2, 3, 2.0, 1.5, HERTZ).unwrap();
        let mut stream = BinaryStream::new();
        yaw.write_real_to_stream(&mut stream);
        stream.rewind();
        let mut restored = YawRotatorController::new(0, 0, 0.0, 1.0, HERTZ).unwrap();
        restored.read_real_from_stream(&mut stream).unwrap();
        assert!(stream.at_end());
        assert!(approx(restored.target_yaw, yaw.target_yaw));
        assert!(approx(restored.radians_per_tick, yaw.radians_per_tick));

        let accel_point = AccelerateToPointController::new(
            3,
            0,
            Vector3::new(4.0, 5.0, 6.0),
            1.0,
            2.0,
            FACING_ACCELERATION,
            true,
            HERTZ,
        );
        let mut stream = BinaryStream::new();
        accel_point.write_real_to_stream(&mut stream);
        stream.rewind();
        let mut restored = AccelerateToPointController::new(
            0,
            0,
            Vector3::ZERO,
            0.0,
            0.0,
            FACING_NONE,
            false,
            HERTZ,
        );
        restored.read_real_from_stream(&mut stream).unwrap();
        assert!(stream.at_end());
        assert_eq!(restored.destination, accel_point.destination);
        assert!(restored.stop_at_destination);
        assert_eq!(restored.core.facing, FACING_ACCELERATION);
        assert!(approx(restored.core.acceleration, 1.0));
        assert!(approx(restored.core.max_speed, 2.0));

        let mut along_path = AccelerateAlongPathController::new(
            4,
            0,
            vec![Vector3::new(1.0, 0.0, 0.0), Vector3::new(2.0, 0.0, 0.0)],
            1.0,
            1.0,
            FACING_NONE,
            HERTZ,
        );
        along_path.progress = 1;
        let mut stream = BinaryStream::new();
        along_path.write_real_to_stream(&mut stream);
        stream.rewind();
        let mut restored = AccelerateAlongPathController::new(
            0,
            0,
            vec![Vector3::ZERO],
            0.0,
            0.0,
            FACING_NONE,
            HERTZ,
        );
        restored.read_real_from_stream(&mut stream).unwrap();
        assert!(stream.at_end());
        assert_eq!(restored.waypoints, along_path.waypoints);
        assert_eq!(restored.progress, 1);

        let mut move_entity = MoveToEntityController::new(5, 0, 42, 1.0, 2.0, true, false, HERTZ);
        move_entity.resolved = true;
        let mut stream = BinaryStream::new();
        move_entity.write_real_to_stream(&mut stream);
        stream.rewind();
        let mut restored = MoveToEntityController::new(0, 0, 0, 0.0, 0.0, true, false, HERTZ);
        restored.read_real_from_stream(&mut stream).unwrap();
        assert!(stream.at_end());
        assert_eq!(restored.dest_entity_id, 42);
        assert!(approx(restored.range, 2.0));
        assert!(!restored.resolved, "the entity pointer is re-resolved");

        let mut accel_entity =
            AccelerateToEntityController::new(6, 0, 43, 1.0, 2.0, FACING_VELOCITY, 3.0, HERTZ);
        accel_entity.resolved = true;
        let mut stream = BinaryStream::new();
        accel_entity.write_real_to_stream(&mut stream);
        stream.rewind();
        let mut restored =
            AccelerateToEntityController::new(0, 0, 0, 0.0, 0.0, FACING_NONE, 0.0, HERTZ);
        restored.read_real_from_stream(&mut stream).unwrap();
        assert!(stream.at_end());
        assert_eq!(restored.dest_entity_id, 43);
        assert!(approx(restored.range, 3.0));

        // A truncated stream reports the error.
        let mut bad = BinaryStream::new();
        bad.write_f32(1.0);
        bad.rewind();
        assert!(restored.read_real_from_stream(&mut bad).is_err());
    }

    /// `EntityState::set_position_and_direction` is the
    /// `RealEntity::newPosition` sample: nothing after one tick, a delta
    /// average after two, and an invalid sample resets to the position.
    #[test]
    fn entity_state_samples_velocity_from_position_changes() {
        let mut state = state_at(Vector3::new(0.0, 0.0, 0.0));

        // Tick 1: dur 1, no sample.
        state.set_position_and_direction(
            Vector3::new(1.0, 0.0, 0.0),
            Direction3D::default(),
            1,
            HERTZ,
        );
        assert_eq!(state.velocity, Vector3::ZERO);

        // Tick 2: dur 2, velocity = (2-0) * 10/2.
        state.set_position_and_direction(
            Vector3::new(2.0, 0.0, 0.0),
            Direction3D::default(),
            2,
            HERTZ,
        );
        assert!(approx_v(state.velocity, Vector3::new(10.0, 0.0, 0.0)));

        // Tick 4: dur 2 again, velocity = (5-2) * 5.
        state.set_position_and_direction(
            Vector3::new(5.0, 0.0, 0.0),
            Direction3D::default(),
            4,
            HERTZ,
        );
        assert!(approx_v(state.velocity, Vector3::new(15.0, 0.0, 0.0)));

        // An invalid sample snaps to the current position instead of
        // measuring across it (real_entity.cpp:843-846).
        let mut state = state_at(Vector3::new(1.0, 1.0, 1.0));
        state.position_sample = INVALID_POSITION;
        state.set_position_and_direction(
            Vector3::new(3.0, 1.0, 1.0),
            Direction3D::default(),
            2,
            HERTZ,
        );
        assert_eq!(state.velocity, Vector3::ZERO);
        assert_eq!(state.position_sample, Vector3::new(3.0, 1.0, 1.0));
    }
}
