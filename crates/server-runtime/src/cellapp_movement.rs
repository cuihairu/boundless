//! The movement-family controllers (`server/cellapp/move_controller.cpp`,
//! `turn_controller.cpp`, `base_acceleration_controller.cpp`,
//! `accelerate_to_point_controller.cpp`): `MoveToPoint`, `MoveToEntity`,
//! `YawRotator` and `AccelerateToPoint`.
//!
//! Their drive is **not** the `TimeQueue` but the `Updatables` registry
//! (`lib/server/updatables.cpp`): `MoveController::startReal` /
//! `BaseAccelerationController::startReal` /
//! `YawRotatorController::startReal` are
//! `MF_ASSERT(entity().isReal())` + `CellApp::instance()
//! .registerForUpdate(this)`, `stopReal` is the `deregisterForUpdate`.
//! `ServerApp::advanceTime` runs `callUpdatables()` after the time
//! increment and before the timer queue, so every registered controller
//! steps once per tick in registration order. The registry iterates the
//! size it started with — a controller attached mid-pass only drives
//! from the next tick — and a controller removed mid-pass leaves a hole
//! that the pass skips (`Updatables::remove` NULLs the slot while
//! `inUpdate_`); outside a pass removal swaps the last slot into the
//! hole.
//!
//! Every movement kind registers under the `"Movement"` exclusive
//! category, so they share one controller id above the mask and replace
//! each other on attach.
//!
//! The shared `MoveController::move` walks `metresPerTick_` a tick
//! (velocity / `updateHertz`, frozen at construction), faces the
//! movement when asked, always clears `isOnGround`, and reports arrival
//! with the float-exact `almostEqual` band (0.0004). The acceleration
//! family instead steers a velocity vector toward a desired velocity
//! (`calculateDesiredVelocity` / `calculateAccelerationVector`) and
//! reads the entity's *sampled* velocity (`RealEntity::newPosition`) as
//! its current speed. `YawRotator` turns
//! `radiansPerTick_` a tick, choosing the anti-clockwise direction when
//! that is shorter (`startReal`'s `isBetween(yaw - MATH_PI, yaw)` check)
//! and turning unconditionally once so a full circle can be requested.
//!
//! Reference quirks kept: `MoveToEntityController::recalcOffset` draws
//! its orbit angle from `rand() % 360` — the port substitutes a fixed
//! xorshift64* stream (the reference seeds libc `rand()` from the clock)
//! so trajectories stay reproducible; `YawRotatorController::New`
//! rejects a non-positive velocity with an AttributeError before any
//! controller exists.

use std::cell::RefCell;
use std::rc::Rc;

use foundation::math::angle::normalise as normalise_angle;
use foundation::math::{MATH_PI, Vector3, almost_zero};

use crate::cellapp_real_entity::{AccelerationFacing, Controller, ControllerKind};
use crate::cellapp_space::{CellApp, Entity};

/// `stdmf.hpp:606`'s default `almostEqual` band.
const ALMOST_EQUAL_EPSILON: f32 = 0.0004;

/// `stdmf_minimal.hpp:136`: `fabsf(v) <= 0.f` — a bit-exact zero test,
/// deliberately *not* an `almostZero`.
fn is_zero(value: f32) -> bool {
    value == 0.0
}

/// The scalar `almostEqual(value, other)` (`stdmf.hpp:606`).
fn almost_equal(value: f32, other: f32) -> bool {
    (value - other).abs() <= ALMOST_EQUAL_EPSILON
}

/// The vector `almostEqual` face: every component in the band.
fn almost_equal_v3(value: Vector3, other: Vector3) -> bool {
    almost_equal(value.x, other.x)
        && almost_equal(value.y, other.y)
        && almost_equal(value.z, other.z)
}

/// `Vector3::unitVector` (`vector3.ipp:295`): a copy run through
/// `normalise` — the almost-zero length guard leaves a zero vector zero
/// instead of dividing by nothing.
fn unit_vector(value: Vector3) -> Vector3 {
    let mut result = value;
    result.normalise();
    result
}

/// `Vector3::projectOnto`: `this` becomes `src` projected onto `dest`'s
/// line. The caller guarantees `dest` is not degenerate (the
/// acceleration path only calls it with a desired velocity above the
/// almost-zero band).
fn project_onto(src: Vector3, dest: Vector3) -> Vector3 {
    let denominator = dest.dot_product(&dest);
    dest * (src.dot_product(&dest) / denominator)
}

/// Per-component division, kept exact against the reference's
/// `newVelocity / CellAppConfig::updateHertz()`.
fn divided_by(value: Vector3, scalar: f32) -> Vector3 {
    Vector3::new(value.x / scalar, value.y / scalar, value.z / scalar)
}

/// `Angle::turnRange` (`angle.ipp`): the positive-direction turn from
/// `from` to `to`, in `[0, 2π)`.
fn turn_range(from: f32, to: f32) -> f32 {
    let difference = to - from;
    if difference < 0.0 {
        difference + MATH_PI * 2.0
    } else {
        difference
    }
}

/// `Angle::isBetween` (`angle.ipp`): whether `angle` sits in the
/// positive-direction arc from `min` to `max`.
fn is_between(angle: f32, min: f32, max: f32) -> bool {
    turn_range(min, angle) <= turn_range(min, max)
}

/// `BaseAccelerationController::calculateDesiredVelocity`: the unit
/// direction to the destination scaled either by `maxSpeed`, or — when
/// stopping there — by the speed an object accelerating from rest over
/// the remaining distance would reach, whichever is smaller.
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
        let speed_at_distance = (2.0 * destination_vector.length() * acceleration).sqrt();
        desired_velocity.scale(max_speed.min(speed_at_distance));
    } else {
        desired_velocity.scale(max_speed);
    }
    desired_velocity
}

/// `BaseAccelerationController::calculateAccelerationVector`: the
/// direction to steer the current velocity toward the desired one —
/// straight along the desired velocity when there is nothing to correct
/// (or no current velocity), straight against the current velocity when
/// the desired one is nothing, and otherwise along
/// `difference + 2 * (difference - its projection on the desired)`.
fn calculate_acceleration_vector(current_velocity: Vector3, desired_velocity: Vector3) -> Vector3 {
    if almost_equal_v3(current_velocity, desired_velocity) {
        return unit_vector(desired_velocity);
    }
    if almost_zero(desired_velocity.length_squared(), ALMOST_EQUAL_EPSILON) {
        return -unit_vector(current_velocity);
    }
    let acceleration_vector = if current_velocity.length_squared() > 0.0 {
        let velocity_difference = desired_velocity - current_velocity;
        let projection = project_onto(velocity_difference, desired_velocity);
        projection + (velocity_difference - projection) * 2.0
    } else {
        desired_velocity
    };
    unit_vector(acceleration_vector)
}

/// `BaseAccelerationController::move`'s facing-vector rules: the vector
/// the entity turns to face, per the `facing_` mode.
fn facing_vector_of(
    facing: AccelerationFacing,
    new_velocity: Vector3,
    velocity_difference: Vector3,
    acceleration_vector: Vector3,
    acceleration_per_tick: f32,
) -> Vector3 {
    if facing == AccelerationFacing::Velocity && new_velocity.length_squared() > 0.0 {
        return unit_vector(new_velocity);
    }
    if facing == AccelerationFacing::Acceleration {
        // Avoid facing at odd angles when the current velocity is almost
        // what we want already.
        if velocity_difference.length() > acceleration_per_tick {
            return acceleration_vector;
        }
        return unit_vector(new_velocity);
    }
    Vector3::ZERO
}

impl CellApp {
    /// `ServerApp::advanceTime`'s `callUpdatables()` (`updatables.cpp`):
    /// step every registered controller once, iterating the size the
    /// pass started with so controllers attached mid-pass wait for the
    /// next tick, then compact the holes.
    pub fn run_movement_pass(&mut self) {
        let size = self.updatable_controllers.len();
        self.in_movement_update = true;
        for index in 0..size {
            self.update_registered_controller(index);
        }
        self.in_movement_update = false;
        self.updatable_controllers.retain(|slot| slot.is_some());
    }

    /// The registry slot at `index`, or nothing when a mid-pass removal
    /// holed it.
    fn update_registered_controller(&mut self, index: usize) {
        let Some((entity, id)) = self.updatable_controllers.get(index).copied().flatten() else {
            return;
        };
        self.update_movement_controller(entity, id);
    }

    /// `CellApp::registerForUpdate` (`server_app.cpp`): the controller
    /// joins the per-tick pass at the end of the registry.
    fn register_updatable(&mut self, entity: u32, id: crate::cellapp_real_entity::ControllerId) {
        self.updatable_controllers.push(Some((entity, id)));
    }

    /// `CellApp::deregisterForUpdate`: outside a pass the slot swaps
    /// with the registry's last entry (`Updatables::remove`'s hole
    /// walk); during a pass it becomes a `None` hole the pass skips.
    pub fn deregister_updatable(
        &mut self,
        entity: u32,
        id: crate::cellapp_real_entity::ControllerId,
    ) {
        let found = self
            .updatable_controllers
            .iter()
            .rposition(|slot| *slot == Some((entity, id)));
        match (found, self.in_movement_update) {
            (Some(index), true) => self.updatable_controllers[index] = None,
            (Some(index), false) => {
                self.updatable_controllers.swap_remove(index);
            }
            (None, _) => {}
        }
    }

    /// The movement families' shared `startReal`
    /// (`registerForUpdate`), plus `YawRotatorController::startReal`'s
    /// extra two steps: the anti-clockwise direction check and the
    /// unconditional first turn.
    pub(crate) fn start_movement_controller(
        &mut self,
        entity_rc: &Rc<RefCell<Entity>>,
        id: crate::cellapp_real_entity::ControllerId,
        kind: ControllerKind,
    ) {
        self.register_updatable(entity_rc.borrow().id, id);
        if let ControllerKind::YawRotator {
            target_yaw,
            radians_per_tick,
        } = kind
        {
            self.start_yaw_rotator(entity_rc, id, target_yaw, radians_per_tick);
        }
    }

    /// `YawRotatorController::startReal`'s body after the registration:
    /// "check if anti-clockwise is shortest direction", write the
    /// sign-flipped `radiansPerTick_` back, then turn once without
    /// checking — the only way a full-circle request starts moving.
    fn start_yaw_rotator(
        &mut self,
        entity_rc: &Rc<RefCell<Entity>>,
        id: crate::cellapp_real_entity::ControllerId,
        target_yaw: f32,
        radians_per_tick: f32,
    ) {
        let current_yaw = entity_rc.borrow().direction[0];
        let minimum = normalise_angle(current_yaw - MATH_PI);
        let radians_per_tick = if is_between(target_yaw, minimum, current_yaw) {
            -radians_per_tick
        } else {
            radians_per_tick
        };
        let mut entity = entity_rc.borrow_mut();
        let slot = entity
            .controllers
            .container
            .get_mut(&id)
            .expect("the yaw rotator was just attached");
        slot.kind = ControllerKind::YawRotator {
            target_yaw,
            radians_per_tick,
        };
        drop(entity);
        self.turn_yaw_rotator(entity_rc, target_yaw, radians_per_tick);
    }

    /// `YawRotatorController::turn`: step the yaw one
    /// `radians_per_tick`, snapping onto the target when the step would
    /// carry past it, and report whether it reached.
    fn turn_yaw_rotator(
        &mut self,
        entity_rc: &Rc<RefCell<Entity>>,
        target_yaw: f32,
        radians_per_tick: f32,
    ) -> bool {
        let current_yaw = entity_rc.borrow().direction[0];
        // `Angle newYaw = direction.yaw + radiansPerTick_`: the float sum
        // normalises through the Angle constructor.
        let mut new_yaw = normalise_angle(current_yaw + radians_per_tick);
        let mut reached_yaw = false;
        let snapped = if radians_per_tick > 0.0 {
            is_between(target_yaw, current_yaw, new_yaw)
        } else {
            is_between(target_yaw, new_yaw, current_yaw)
        };
        if snapped {
            new_yaw = target_yaw;
            reached_yaw = true;
        }
        let (position, mut direction) = {
            let entity = entity_rc.borrow();
            (
                Vector3::new(entity.position[0], entity.position[1], entity.position[2]),
                Vector3::new(
                    entity.direction[0],
                    entity.direction[1],
                    entity.direction[2],
                ),
            )
        };
        direction.x = normalise_angle(new_yaw);
        self.set_entity_position_and_direction(entity_rc, position, direction);
        reached_yaw
    }

    /// `MoveController::move` (`move_controller.cpp:170`): walk
    /// `metres_per_tick` toward `destination` — landing exactly on it
    /// when the step would overshoot, else advancing the normalised
    /// movement vector; face the movement when asked; drop below the
    /// navigation clamp when configured; clear `isOnGround`; report
    /// arrival. `remaining` carries the reference's `pRemainingTicks`
    /// budget (the navigation families spend it across waypoints,
    /// BW-08-02-04; these families pass none).
    #[allow(clippy::too_many_arguments)]
    fn move_towards(
        &mut self,
        entity_rc: &Rc<RefCell<Entity>>,
        metres_per_tick: f32,
        face_movement: bool,
        move_vertically: bool,
        destination: Vector3,
        remaining: Option<&mut f32>,
    ) -> bool {
        let (mut position, mut direction) = {
            let entity = entity_rc.borrow();
            (
                Vector3::new(entity.position[0], entity.position[1], entity.position[2]),
                Vector3::new(
                    entity.direction[0],
                    entity.direction[1],
                    entity.direction[2],
                ),
            )
        };
        let mut metres_to_go = metres_per_tick;
        if let Some(remaining) = remaining.as_deref() {
            metres_to_go = *remaining * metres_per_tick;
        }

        let mut movement = destination - position;
        if !move_vertically {
            movement.y = 0.0;
        }

        // If we are real close, then finish up: land verbatim on the
        // destination and scale the remaining-ticks budget by the
        // un-walked fraction; otherwise keep walking and zero it.
        let remaining_factor = if movement.length() < metres_to_go {
            position = destination;
            (metres_to_go - movement.length()) / metres_per_tick
        } else {
            movement.normalise();
            movement.scale(metres_to_go);
            position = position + movement;
            0.0
        };
        if let Some(remaining) = remaining {
            *remaining *= remaining_factor;
        }

        // Make sure we are facing the right direction.
        if face_movement {
            if !is_zero(movement.x) || !is_zero(movement.z) {
                direction.x = normalise_angle(movement.yaw());
            }
            if !is_zero(movement.y) {
                direction.y = normalise_angle(movement.pitch());
            }
        }

        // `CellAppConfig::shouldNavigationDropPosition` (default false):
        // push the position below any navmesh overhang the navigation
        // families would have walked under.
        if self.should_navigation_drop_position {
            position.y +=
                self.navigation_max_climb + movement.length() * self.navigation_max_slope_gradient;
        }

        // No longer on the ground...
        entity_rc.borrow_mut().is_on_ground = false;
        self.set_entity_position_and_direction(entity_rc, position, direction);
        almost_equal(position.x, destination.x)
            && almost_equal(position.z, destination.z)
            && (almost_equal(position.y, destination.y) || !move_vertically)
    }

    /// `MoveToEntityController::update` (`move_controller.cpp:432`):
    /// resolve the target lazily and draw the orbit offset exactly once,
    /// walk toward the offset point, and when inside `range` face the
    /// target exactly and finish. A missing or destroyed target ends the
    /// controller with `onMoveFailure`.
    #[allow(clippy::too_many_arguments)]
    fn update_move_to_entity(
        &mut self,
        entity_rc: &Rc<RefCell<Entity>>,
        entity: u32,
        id: crate::cellapp_real_entity::ControllerId,
        target: u32,
        range: f32,
        metres_per_tick: f32,
        face_movement: bool,
        move_vertically: bool,
        offset: [f32; 2],
        resolved: bool,
    ) {
        // `if (!pDestEntity_) { pDestEntity_ = findEntity(...);
        // recalcOffset(); }` — one resolve, one offset draw.
        let (offset, resolved) = if resolved {
            (offset, resolved)
        } else {
            (self.recalc_move_offset(range), true)
        };
        let user_arg = entity_rc
            .borrow()
            .controllers
            .container
            .get(&id)
            .map(|controller| controller.user_arg)
            .unwrap_or(0);
        if let Some(slot) = entity_rc.borrow_mut().controllers.container.get_mut(&id) {
            slot.kind = ControllerKind::MoveToEntity {
                target,
                range,
                metres_per_tick,
                face_movement,
                move_vertically,
                offset,
                resolved,
            };
        }

        // `findEntity` + the destroyed check: a target that is gone from
        // the population or destroyed ends the controller.
        let destination_rc = self
            .entity_population
            .get(&target)
            .filter(|candidate| !candidate.borrow().destroyed)
            .cloned();
        let Some(destination_rc) = destination_rc else {
            self.entity_callback(entity, "onMoveFailure", false, Some((id, user_arg)));
            if entity_rc.borrow().controllers.container.contains_key(&id) {
                self.cancel_controller(entity, id);
            }
            return;
        };

        let destination = {
            let target_entity = destination_rc.borrow();
            Vector3::new(
                target_entity.position[0],
                target_entity.position[1],
                target_entity.position[2],
            )
        };
        let current_position = {
            let entity = entity_rc.borrow();
            Vector3::new(entity.position[0], entity.position[1], entity.position[2])
        };
        let mut remaining = destination - current_position;
        let mut destination = destination;
        if !move_vertically {
            // 2D distance projected onto the x-z plane.
            remaining.y = 0.0;
            destination.y = current_position.y;
        }
        let close_enough = remaining.length() < range;

        // move(..) only happens in local space; without a vehicle the
        // destination stays world space (the vehicle transform is
        // BW-08-02-05).
        let destination_local = destination + Vector3::new(offset[0], 0.0, offset[1]);
        if close_enough
            || self.move_towards(
                entity_rc,
                metres_per_tick,
                face_movement,
                move_vertically,
                destination_local,
                None,
            )
        {
            if close_enough {
                // In case we were moving towards a point near the
                // entity, make sure that we are now facing it exactly.
                let position = {
                    let entity = entity_rc.borrow();
                    Vector3::new(entity.position[0], entity.position[1], entity.position[2])
                };
                let vector = destination - position;
                let mut direction = {
                    let entity = entity_rc.borrow();
                    Vector3::new(
                        entity.direction[0],
                        entity.direction[1],
                        entity.direction[2],
                    )
                };
                if face_movement {
                    direction.x = normalise_angle(vector.yaw());
                }
                self.set_entity_position_and_direction(entity_rc, position, direction);
            }
            self.finish_movement_callback(entity_rc, entity, id, "onMove");
        }
    }

    /// `BaseAccelerationController::move`
    /// (`base_acceleration_controller.cpp:237`): steer the entity's
    /// velocity toward the desired one under the acceleration clamp,
    /// step by it, face per the facing mode, and report arrival — with
    /// the almost-stopped extra condition when stopping at the
    /// destination.
    fn accelerate_move(
        &mut self,
        entity_rc: &Rc<RefCell<Entity>>,
        acceleration: f32,
        max_speed: f32,
        facing: AccelerationFacing,
        stop_at_destination: bool,
        destination: [f32; 3],
    ) -> bool {
        let destination = Vector3::new(destination[0], destination[1], destination[2]);
        let current_velocity = self.refresh_current_velocity(entity_rc);
        let (position, direction) = {
            let entity = entity_rc.borrow();
            (
                Vector3::new(entity.position[0], entity.position[1], entity.position[2]),
                Vector3::new(
                    entity.direction[0],
                    entity.direction[1],
                    entity.direction[2],
                ),
            )
        };
        let mut desired_velocity = calculate_desired_velocity(
            position,
            destination,
            acceleration,
            max_speed,
            stop_at_destination,
        );
        if desired_velocity.length() > max_speed {
            desired_velocity.normalise();
            desired_velocity.scale(max_speed);
        }
        let acceleration_vector = calculate_acceleration_vector(current_velocity, desired_velocity);
        let destination_vector = destination - position;
        let velocity_difference = current_velocity - desired_velocity;
        let acceleration_per_tick = acceleration / self.update_hertz as f32;

        let new_velocity = if velocity_difference.length() > acceleration_per_tick {
            current_velocity + acceleration_vector * acceleration_per_tick
        } else {
            desired_velocity
        };

        // If we are real close, then finish up.
        let mut position = position;
        let step_distance = new_velocity.length() / self.update_hertz as f32;
        if step_distance > destination_vector.length() {
            position = destination;
        } else {
            position = position + divided_by(new_velocity, self.update_hertz as f32);
        }

        // Make sure we are facing the right direction.
        let mut direction = direction;
        if facing != AccelerationFacing::None {
            let facing_vector = facing_vector_of(
                facing,
                new_velocity,
                velocity_difference,
                acceleration_vector,
                acceleration_per_tick,
            );
            if !is_zero(facing_vector.x) || !is_zero(facing_vector.z) {
                direction.x = normalise_angle(facing_vector.yaw());
            }
            if !is_zero(facing_vector.y) {
                direction.y = normalise_angle(facing_vector.pitch());
            }
        }

        // No longer on the ground...
        entity_rc.borrow_mut().is_on_ground = false;
        self.set_entity_position_and_direction(entity_rc, position, direction);
        if stop_at_destination {
            almost_equal_v3(position, destination)
                && almost_zero(current_velocity.length_squared(), ALMOST_EQUAL_EPSILON)
        } else {
            almost_equal_v3(position, destination)
        }
    }

    /// `BaseAccelerationController::refreshCurrentVelocity`'s unmounted
    /// face: `currVelocity_ = pReal()->velocity()` — the entity's
    /// sampled velocity. The per-vehicle sampling branches are
    /// BW-08-02-05.
    fn refresh_current_velocity(&mut self, entity_rc: &Rc<RefCell<Entity>>) -> Vector3 {
        let entity = entity_rc.borrow();
        Vector3::new(entity.velocity[0], entity.velocity[1], entity.velocity[2])
    }

    /// `RealEntity::setPositionAndDirection` + `newPosition`
    /// (`real_entity.cpp:831`): store the position (direction yaw/pitch
    /// normalising through the `Angle` assignment), then re-derive the
    /// sampled velocity — but only after a full tick of separation
    /// (`dur > 1`), keeping the older sample otherwise.
    pub fn set_entity_position_and_direction(
        &mut self,
        entity_rc: &Rc<RefCell<Entity>>,
        position: Vector3,
        direction: Vector3,
    ) {
        let mut entity = entity_rc.borrow_mut();
        entity.position = [position.x, position.y, position.z];
        entity.direction = [
            normalise_angle(direction.x),
            normalise_angle(direction.y),
            direction.z,
        ];
        let duration = self.time.wrapping_sub(entity.position_sample_time);
        if duration > 1 {
            let time_inverse = self.update_hertz as f32 / duration as f32;
            entity.velocity = [
                (position.x - entity.position_sample[0]) * time_inverse,
                (position.y - entity.position_sample[1]) * time_inverse,
                (position.z - entity.position_sample[2]) * time_inverse,
            ];
            entity.position_sample = entity.position;
            entity.position_sample_time = self.time;
        }
    }

    /// `MoveToEntityController::recalcOffset`: the orbit point at
    /// `range` distance, at a `rand() % 360` angle. The reference draws
    /// from libc `rand()`; the port draws from its fixed xorshift64*
    /// stream.
    fn recalc_move_offset(&mut self, range: f32) -> [f32; 2] {
        let angle = (self.next_movement_rand() % 360) as f32 * (MATH_PI / 180.0);
        [range * angle.cos(), range * angle.sin()]
    }

    /// The next draw of the port's `rand()` replacement (xorshift64*,
    /// high 32 bits).
    pub fn next_movement_rand(&mut self) -> u32 {
        let mut state = self.movement_rng;
        state ^= state >> 12;
        state ^= state << 25;
        state ^= state >> 27;
        self.movement_rng = state;
        (state.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 32) as u32
    }

    /// The movement pass dispatch (`Controllers::update`'s virtual
    /// `update()`): one step of the family's drive, then the standard
    /// callback and self-cancel on completion.
    // The lint's suggested match guard cannot exist here: guards cannot
    // call the `&mut self` drive methods.
    #[allow(clippy::collapsible_match)]
    fn update_movement_controller(
        &mut self,
        entity: u32,
        id: crate::cellapp_real_entity::ControllerId,
    ) {
        let Some(entity_rc) = self.entity_population.get(&entity).cloned() else {
            return;
        };
        let kind = entity_rc
            .borrow()
            .controllers
            .container
            .get(&id)
            .map(|controller| controller.kind);
        let Some(kind) = kind else {
            return;
        };
        match kind {
            ControllerKind::MoveToPoint {
                destination,
                metres_per_tick,
                face_movement,
                move_vertically,
            } => {
                if self.move_towards(
                    &entity_rc,
                    metres_per_tick,
                    face_movement,
                    move_vertically,
                    Vector3::new(destination[0], destination[1], destination[2]),
                    None,
                ) {
                    self.finish_movement_callback(&entity_rc, entity, id, "onMove");
                }
            }
            ControllerKind::MoveToEntity {
                target,
                range,
                metres_per_tick,
                face_movement,
                move_vertically,
                offset,
                resolved,
            } => self.update_move_to_entity(
                &entity_rc,
                entity,
                id,
                target,
                range,
                metres_per_tick,
                face_movement,
                move_vertically,
                offset,
                resolved,
            ),
            ControllerKind::YawRotator {
                target_yaw,
                radians_per_tick,
            } => {
                if self.turn_yaw_rotator(&entity_rc, target_yaw, radians_per_tick) {
                    self.finish_movement_callback(&entity_rc, entity, id, "onTurn");
                }
            }
            ControllerKind::AccelerateToPoint {
                destination,
                acceleration: a,
                max_speed: ms,
                facing: f,
                stop_at_destination: stop,
            } => {
                if self.accelerate_move(&entity_rc, a, ms, f, stop, destination) {
                    self.finish_movement_callback(&entity_rc, entity, id, "onMove");
                }
            }
            // Only the timer drives and the exclusive placeholders reach
            // the registry dispatch without a movement family behind
            // them; both hang inert by contract.
            inert_kind => {
                debug_assert!(matches!(
                    inert_kind,
                    ControllerKind::Timer { .. } | ControllerKind::Exclusive { .. }
                ));
            }
        }
    }

    /// `Controller::standardCallback` + the post-callback self-cancel:
    /// the script call records `(controllerID, userArg)`, and while the
    /// controller is still attached it cancels itself (the reference
    /// holds a `ControllerPtr` keep-alive across the call).
    fn finish_movement_callback(
        &mut self,
        entity_rc: &Rc<RefCell<Entity>>,
        entity: u32,
        id: crate::cellapp_real_entity::ControllerId,
        name: &'static str,
    ) {
        let user_arg = entity_rc
            .borrow()
            .controllers
            .container
            .get(&id)
            .map(|controller| controller.user_arg)
            .unwrap_or(0);
        self.entity_callback(entity, name, false, Some((id, user_arg)));
        if entity_rc.borrow().controllers.container.contains_key(&id) {
            self.cancel_controller(entity, id);
        }
    }

    /// `YawRotatorController::New` + `Entity.addYawRotator`: reject a
    /// non-positive velocity with the AttributeError face (the port
    /// records it on the script-error ledger) before any controller
    /// exists, else attach through the generic face.
    pub fn attach_yaw_rotator(
        &mut self,
        entity: u32,
        target_yaw: f32,
        velocity: f32,
        user_arg: i32,
    ) -> crate::cellapp_real_entity::ControllerId {
        if velocity <= 0.0 {
            self.script_errors.push((
                entity,
                "Can't add YawRotator controller with 0 or negative velocity",
            ));
            return 0;
        }
        let controller = Controller {
            kind: ControllerKind::yaw_rotator(target_yaw, velocity, self.update_hertz),
            user_arg,
        };
        self.attach_controller(entity, controller)
    }
}

// ---------------------------------------------------------------------------
// Section: tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cellapp_real_entity::{
        ADD_ENTITY_DATA_IS_ON_GROUND, ControllerId, CreateRealOutcome, EntityScript,
        MOVEMENT_CATEGORY,
    };
    use crate::cellapp_space::Space;
    use crate::cellappmgr::SpaceId;
    use std::collections::BTreeMap;

    const SID: SpaceId = 3;
    const TYPE: u32 = 7;
    const MOVEMENT: ControllerId = 0x8000;

    /// The 44-byte creation stream, at a chosen position: id + type id,
    /// position, flags, direction, last event number, base address,
    /// witness.
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

    /// A CellApp with the fixture type registered (plus `extra` methods).
    fn app_with(extra: &[&'static str]) -> CellApp {
        let mut app = CellApp::default();
        let mut script = EntityScript::with_init(Vec::new());
        for name in extra {
            script.methods.insert(name);
        }
        app.entity_scripts.insert(TYPE, script);
        app
    }

    /// `spaces_with`: the single fixture space.
    fn spaces_with() -> BTreeMap<SpaceId, Space> {
        let mut spaces = BTreeMap::new();
        spaces.insert(SID, Space::new(SID));
        spaces
    }

    /// Create entity `id` at `position` with the fixture type.
    fn create(
        app: &mut CellApp,
        spaces: &mut BTreeMap<SpaceId, Space>,
        id: u32,
        position: [f32; 3],
    ) {
        let outcome = app.create_real_entity(spaces, SID, &create_stream(id, TYPE, position));
        assert_eq!(outcome, CreateRealOutcome::Created(id));
        // The creation flush records `__init__`; these tests count only
        // their own movement callbacks.
        app.script_calls.clear();
    }

    #[test]
    fn move_to_point_walks_a_fixed_trajectory() {
        let mut app = app_with(&["onMove"]);
        let mut spaces = spaces_with();
        create(&mut app, &mut spaces, 77, [0.0, 0.0, 0.0]);

        // 5 m/s at hertz 10 is exactly half a metre a tick: twenty even
        // steps from the origin to (10, 0, 0).
        let id = app.attach_controller(
            77,
            Controller {
                kind: ControllerKind::move_to_point([10.0, 0.0, 0.0], 5.0, true, false, 10),
                user_arg: 4,
            },
        );
        assert_eq!(id, MOVEMENT);
        let entity = app.entity_population[&77].borrow();
        assert_eq!(
            entity.controllers.container[&MOVEMENT].kind,
            ControllerKind::MoveToPoint {
                destination: [10.0, 0.0, 0.0],
                metres_per_tick: 0.5,
                face_movement: true,
                move_vertically: false,
            }
        );
        drop(entity);

        let half_pi = MATH_PI / 2.0;
        for tick in 1..=19 {
            app.advance_tick();
            let entity = app.entity_population[&77].borrow();
            assert_eq!(entity.position, [0.5 * tick as f32, 0.0, 0.0]);
            // Facing the +x movement: yaw atan2(1, 0).
            assert_eq!(entity.direction[0], half_pi);
            assert!(!entity.is_on_ground);
            drop(entity);
            assert!(app.script_calls.is_empty());
            assert_eq!(app.updatable_controllers, vec![Some((77, MOVEMENT))]);
        }
        // The velocity sampler re-arms every second tick: two ticks of
        // one-metre separation at hertz 10 read 5 m/s.
        assert_eq!(
            app.entity_population[&77].borrow().velocity,
            [5.0, 0.0, 0.0]
        );

        // Tick 20: half a metre to go is not *less* than half a metre,
        // so the walk branch lands exactly on the destination.
        app.advance_tick();
        let entity = app.entity_population[&77].borrow();
        assert_eq!(entity.position, [10.0, 0.0, 0.0]);
        assert!(!entity.controllers.container.contains_key(&MOVEMENT));
        drop(entity);
        assert_eq!(app.updatable_controllers, Vec::new());
        assert_eq!(app.script_calls.len(), 1);
        assert_eq!(app.script_calls[0].entity, 77);
        assert_eq!(app.script_calls[0].name, "onMove");
        assert_eq!(app.script_calls[0].timer, Some((MOVEMENT, 4)));

        // Cancelled: the later ticks do nothing.
        app.advance_tick();
        assert_eq!(
            app.entity_population[&77].borrow().position,
            [10.0, 0.0, 0.0]
        );
        assert_eq!(app.script_calls.len(), 1);
    }

    #[test]
    fn move_to_point_lands_exactly_on_a_near_destination() {
        let mut app = app_with(&["onMove"]);
        let mut spaces = spaces_with();
        create(&mut app, &mut spaces, 77, [0.0, 0.0, 0.0]);

        // A third of a tick's walk away: the arrival branch copies the
        // destination over verbatim.
        let id = app.attach_controller(
            77,
            Controller {
                kind: ControllerKind::move_to_point([0.3, 0.0, 0.0], 5.0, true, false, 10),
                user_arg: 0,
            },
        );
        assert_eq!(id, MOVEMENT);
        app.advance_tick();
        let entity = app.entity_population[&77].borrow();
        assert_eq!(entity.position, [0.3, 0.0, 0.0]);
        assert_eq!(entity.direction[0], MATH_PI / 2.0);
        assert!(!entity.controllers.container.contains_key(&MOVEMENT));
        drop(entity);
        assert_eq!(app.script_calls.len(), 1);
    }

    #[test]
    fn move_to_point_horizontal_gait_ignores_height() {
        let mut app = app_with(&["onMove"]);
        let mut spaces = spaces_with();
        create(&mut app, &mut spaces, 77, [0.0, 5.0, 0.0]);

        // moveVertically off: the y of the movement zeroes out, the walk
        // is planar, and arrival tolerates the unmet height.
        app.attach_controller(
            77,
            Controller {
                kind: ControllerKind::move_to_point([2.0, 9.0, 0.0], 2.0, true, false, 10),
                user_arg: 0,
            },
        );
        for tick in 1..=9 {
            app.advance_tick();
            let position = app.entity_population[&77].borrow().position;
            // 0.2 is not a power of two: the walk accumulates, so
            // compare within the accumulation's rounding reach.
            assert!((position[0] - 0.2 * tick as f32).abs() < 1e-5);
            assert_eq!(position[1], 5.0);
        }
        // The tenth step slips under the almost-equal band: the arrival
        // branch copies the destination VERBATIM — unmet height and all
        // — so the entity finishes at the destination's y.
        app.advance_tick();
        let entity = app.entity_population[&77].borrow();
        assert_eq!(entity.position, [2.0, 9.0, 0.0]);
        assert!(entity.controllers.container.is_empty());
        drop(entity);
        assert_eq!(app.script_calls.len(), 1);
    }

    #[test]
    fn move_to_point_vertical_gait_climbs_and_pitches() {
        let mut app = app_with(&["onMove"]);
        let mut spaces = spaces_with();
        create(&mut app, &mut spaces, 77, [0.0, 0.0, 0.0]);

        // moveVertically on: a straight-up walk sets the pitch
        // (-atan2(1, 0)) and leaves the zero x/z yaw untouched.
        app.attach_controller(
            77,
            Controller {
                kind: ControllerKind::move_to_point([0.0, 4.0, 0.0], 4.0, true, true, 10),
                user_arg: 0,
            },
        );
        app.advance_tick();
        let entity = app.entity_population[&77].borrow();
        assert_eq!(entity.position, [0.0, 0.4, 0.0]);
        assert_eq!(entity.direction[0], 0.0);
        assert_eq!(entity.direction[1], -MATH_PI / 2.0);
        drop(entity);
        assert!(app.script_calls.is_empty());
    }

    #[test]
    fn movement_controllers_replace_each_other_on_the_exclusive_id() {
        let mut app = app_with(&["onMove", "onTurn"]);
        let mut spaces = spaces_with();
        create(&mut app, &mut spaces, 77, [0.0, 0.0, 0.0]);

        let first = app.attach_controller(
            77,
            Controller {
                kind: ControllerKind::move_to_point([10.0, 0.0, 0.0], 5.0, true, false, 10),
                user_arg: 1,
            },
        );
        assert_eq!(first, MOVEMENT);
        assert_eq!(app.get_exclusive_id(MOVEMENT_CATEGORY), MOVEMENT);

        // The turn factory replaces the walker under the same id; the
        // registry keeps one live slot and the container one entry.
        let second = app.attach_yaw_rotator(77, MATH_PI, 10.0, 6);
        assert_eq!(second, MOVEMENT);
        let entity = app.entity_population[&77].borrow();
        assert_eq!(entity.controllers.container.len(), 1);
        assert!(matches!(
            entity.controllers.container[&MOVEMENT].kind,
            ControllerKind::YawRotator { .. }
        ));
        assert_eq!(entity.controllers.dels, 1);
        drop(entity);
        // startReal turned once unconditionally — but the reference
        // ignores that turn's result: the -1.0 step of the flipped
        // rotation did not reach the target, so the controller lives on.
        assert_eq!(app.updatable_controllers, vec![Some((77, MOVEMENT))]);
        assert_eq!(
            app.entity_population[&77].borrow().direction[0],
            normalise_angle(-1.0)
        );
        assert!(app.script_calls.is_empty());
    }

    #[test]
    fn yaw_rotator_rejects_non_positive_velocity() {
        let mut app = app_with(&["onTurn"]);
        let mut spaces = spaces_with();
        create(&mut app, &mut spaces, 77, [0.0, 0.0, 0.0]);

        assert_eq!(app.attach_yaw_rotator(77, 1.0, 0.0, 0), 0);
        assert_eq!(app.attach_yaw_rotator(77, 1.0, -1.0, 0), 0);
        assert_eq!(
            app.script_errors,
            vec![
                (
                    77,
                    "Can't add YawRotator controller with 0 or negative velocity"
                ),
                (
                    77,
                    "Can't add YawRotator controller with 0 or negative velocity"
                ),
            ]
        );
        assert!(
            app.entity_population[&77]
                .borrow()
                .controllers
                .container
                .is_empty()
        );

        // A positive velocity attaches.
        assert_ne!(app.attach_yaw_rotator(77, 1.0, 1.0, 0), 0);
    }

    #[test]
    fn yaw_rotator_turns_anticlockwise_when_that_is_shorter() {
        let mut app = app_with(&["onTurn"]);
        let mut spaces = spaces_with();
        create(&mut app, &mut spaces, 77, [0.0, 0.0, 0.0]);

        // Facing 0 and told to face -1: the anti-clockwise check
        // (isBetween(yaw - π, yaw)) flips the step so the turn goes the
        // short way, and startReal turns once unconditionally.
        let id = app.attach_yaw_rotator(77, -1.0, 2.0, 3);
        assert_eq!(id, MOVEMENT);
        let entity = app.entity_population[&77].borrow();
        assert!(matches!(&entity.controllers.container[&MOVEMENT].kind,
            ControllerKind::YawRotator { radians_per_tick, .. } if *radians_per_tick < 0.0));
        drop(entity);
        assert_eq!(
            app.entity_population[&77].borrow().direction[0],
            normalise_angle(-0.2)
        );
        assert!(app.script_calls.is_empty());

        // -0.4, -0.6, -0.8, then the -1.0 snap onto the target with the
        // onTurn call and the self-cancel.
        let expected = [-0.4f32, -0.6, -0.8];
        for yaw in expected {
            app.advance_tick();
            let entity = app.entity_population[&77].borrow();
            assert!((entity.direction[0] - yaw).abs() < 1e-6);
            assert!(entity.controllers.container.contains_key(&MOVEMENT));
            drop(entity);
        }
        app.advance_tick();
        let entity = app.entity_population[&77].borrow();
        assert_eq!(entity.direction[0], -1.0);
        assert!(!entity.controllers.container.contains_key(&MOVEMENT));
        drop(entity);
        assert_eq!(app.script_calls.len(), 1);
        assert_eq!(app.script_calls[0].timer, Some((MOVEMENT, 3)));
    }

    #[test]
    fn yaw_rotator_keeps_the_clockwise_step_when_that_is_shorter() {
        let mut app = app_with(&["onTurn"]);
        let mut spaces = spaces_with();
        create(&mut app, &mut spaces, 77, [0.0, 0.0, 0.0]);

        // Facing 0 and told to face +1: no flip, the positive steps walk
        // to the same snap.
        app.attach_yaw_rotator(77, 1.0, 2.0, 0);
        let entity = app.entity_population[&77].borrow();
        assert!(matches!(&entity.controllers.container[&MOVEMENT].kind,
            ControllerKind::YawRotator { radians_per_tick, .. } if *radians_per_tick > 0.0));
        drop(entity);
        assert_eq!(
            app.entity_population[&77].borrow().direction[0],
            normalise_angle(0.2)
        );
        for yaw in [0.4f32, 0.6, 0.8] {
            app.advance_tick();
            let entity = app.entity_population[&77].borrow();
            assert!((entity.direction[0] - yaw).abs() < 1e-6);
            drop(entity);
        }
        app.advance_tick();
        let entity = app.entity_population[&77].borrow();
        assert_eq!(entity.direction[0], 1.0);
        assert!(!entity.controllers.container.contains_key(&MOVEMENT));
        drop(entity);
        assert_eq!(app.script_calls.len(), 1);
    }

    #[test]
    fn yaw_rotator_already_facing_snaps_on_attach() {
        let mut app = app_with(&["onTurn"]);
        let mut spaces = spaces_with();
        create(&mut app, &mut spaces, 77, [0.0, 0.0, 0.0]);

        // startReal's unconditional first turn is what lets a full-circle
        // request (the target equals the facing) fire at all.
        app.entity_population[&77].borrow_mut().direction[0] = 0.25;
        app.attach_yaw_rotator(77, 0.25, 2.0, 0);
        // The unconditional first turn snapped onto the target at once,
        // but startReal ignores turn()'s result: onTurn and the cancel
        // wait for the first update.
        let entity = app.entity_population[&77].borrow();
        assert_eq!(entity.direction[0], 0.25);
        assert!(entity.controllers.container.contains_key(&MOVEMENT));
        drop(entity);
        assert_eq!(app.updatable_controllers, vec![Some((77, MOVEMENT))]);
        assert!(app.script_calls.is_empty());
        app.advance_tick();
        let entity = app.entity_population[&77].borrow();
        assert_eq!(entity.direction[0], 0.25);
        assert!(entity.controllers.container.is_empty());
        drop(entity);
        assert_eq!(app.script_calls.len(), 1);
        assert_eq!(app.script_calls[0].name, "onTurn");
    }

    #[test]
    fn move_to_entity_walks_the_seeded_orbit_offset() {
        let mut app = app_with(&["onMove"]);
        let mut spaces = spaces_with();
        create(&mut app, &mut spaces, 77, [0.0, 0.0, 0.0]);
        create(&mut app, &mut spaces, 78, [10.0, 0.0, 0.0]);

        // Pin the stream: replicate the xorshift64* step over the
        // current state — the draw the controller will make on its
        // first update — so the trajectory is a fixed input.
        let range = 1.0;
        let mut expected_state = app.movement_rng;
        expected_state ^= expected_state >> 12;
        expected_state ^= expected_state << 25;
        expected_state ^= expected_state >> 27;
        let drawn = ((expected_state.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 32) as u32) % 360;
        let angle = drawn as f32 * (MATH_PI / 180.0);
        let offset = [range * angle.cos(), range * angle.sin()];

        let id = app.attach_controller(
            77,
            Controller {
                kind: ControllerKind::move_to_entity(78, range, 5.0, true, false, 10),
                user_arg: 0,
            },
        );
        assert_eq!(id, MOVEMENT);
        app.advance_tick();
        // The resolve happened on the first update: the offset is drawn
        // exactly once and frozen in the controller.
        let entity = app.entity_population[&77].borrow();
        assert!(matches!(&entity.controllers.container[&MOVEMENT].kind,
            ControllerKind::MoveToEntity { offset: drawn, resolved: true, .. }
                if (drawn[0] - offset[0]).abs() < 1e-6 && (drawn[1] - offset[1]).abs() < 1e-6));
        drop(entity);

        // Each walk step is exactly half a metre, straight toward
        // destination + offset: the travelled distance grows 0.5 a tick
        // from the origin, and the path never leaves the fixed line.
        let goal = [10.0 + offset[0], 0.0, offset[1]];
        let goal_vector = Vector3::new(goal[0], goal[1], goal[2]);
        let direction = {
            let mut unit = goal_vector;
            unit.normalise();
            unit
        };
        // Tick 1 (above) already walked the first step; keep counting
        // from there.
        let mut tick = 1;
        loop {
            app.advance_tick();
            tick += 1;
            assert!(tick < 40, "walked {tick} ticks without arriving");
            let still_walking = app.entity_population[&77]
                .borrow()
                .controllers
                .container
                .contains_key(&MOVEMENT);
            if !still_walking {
                break;
            }
            let entity = app.entity_population[&77].borrow();
            let step = Vector3::new(entity.position[0], entity.position[1], entity.position[2]);
            let length_error = (step.length() - 0.5 * tick as f32).abs();
            assert!(
                length_error < 1e-3,
                "tick {tick}: step {:?} length {} expected {}",
                [step.x, step.y, step.z],
                step.length(),
                0.5 * tick as f32
            );
            // Still on the fixed line: cross product stays zero.
            assert!(Vector3::cross_product(&step, &step, &direction).length() < 1e-4);
            drop(entity);
        }

        // The walk ends on the arrival (onMove) with the controller
        // gone, landed verbatim on the orbit point.
        let entity = app.entity_population[&77].borrow();
        assert_eq!(entity.position, goal);
        drop(entity);
        assert_eq!(app.script_calls.len(), 1);
        assert_eq!(app.script_calls[0].name, "onMove");
    }

    #[test]
    fn move_to_entity_faces_the_target_when_inside_range() {
        let mut app = app_with(&["onMove"]);
        let mut spaces = spaces_with();
        create(&mut app, &mut spaces, 77, [0.0, 0.0, 0.0]);
        create(&mut app, &mut spaces, 78, [0.5, 0.0, 0.0]);

        // A target half a metre away with a range of one: the
        // close-enough face fires on the very first update, before any
        // walking can start — so the orbit offset never moves it.
        app.attach_controller(
            77,
            Controller {
                kind: ControllerKind::move_to_entity(78, 1.0, 5.0, true, false, 10),
                user_arg: 0,
            },
        );
        app.advance_tick();
        assert!(
            !app.entity_population[&77]
                .borrow()
                .controllers
                .container
                .contains_key(&MOVEMENT)
        );
        let entity = app.entity_population[&77].borrow();
        // Still at the origin: closeEnough faces the target exactly
        // without moving — +x, so yaw atan2(0.5, 0).
        assert_eq!(entity.position, [0.0, 0.0, 0.0]);
        assert_eq!(entity.direction[0], MATH_PI / 2.0);
        drop(entity);
        assert_eq!(app.script_calls.len(), 1);
        assert_eq!(app.script_calls[0].name, "onMove");
    }

    #[test]
    fn move_to_entity_without_a_target_fails_on_the_first_update() {
        let mut app = app_with(&["onMoveFailure"]);
        let mut spaces = spaces_with();
        create(&mut app, &mut spaces, 77, [0.0, 0.0, 0.0]);

        app.attach_controller(
            77,
            Controller {
                kind: ControllerKind::move_to_entity(999, 1.0, 5.0, true, false, 10),
                user_arg: 2,
            },
        );
        assert_eq!(app.updatable_controllers, vec![Some((77, MOVEMENT))]);
        app.advance_tick();
        let entity = app.entity_population[&77].borrow();
        assert_eq!(entity.position, [0.0, 0.0, 0.0]);
        assert!(entity.controllers.container.is_empty());
        drop(entity);
        assert_eq!(app.updatable_controllers, Vec::new());
        assert_eq!(app.script_calls.len(), 1);
        assert_eq!(app.script_calls[0].name, "onMoveFailure");
        assert_eq!(app.script_calls[0].timer, Some((MOVEMENT, 2)));
    }

    #[test]
    fn move_to_entity_destroyed_target_fails_mid_flight() {
        let mut app = app_with(&["onMove", "onMoveFailure"]);
        let mut spaces = spaces_with();
        create(&mut app, &mut spaces, 77, [0.0, 0.0, 0.0]);
        create(&mut app, &mut spaces, 78, [10.0, 0.0, 0.0]);

        app.attach_controller(
            77,
            Controller {
                kind: ControllerKind::move_to_entity(78, 1.0, 5.0, true, false, 10),
                user_arg: 0,
            },
        );
        for _ in 0..4 {
            app.advance_tick();
        }
        let walked = app.entity_population[&77].borrow().position;
        assert!(walked[0] > 0.0);

        app.destroy_entity(78);
        app.advance_tick();
        let entity = app.entity_population[&77].borrow();
        assert_eq!(entity.position, walked);
        assert!(entity.controllers.container.is_empty());
        drop(entity);
        assert_eq!(app.script_calls.len(), 1);
        assert_eq!(app.script_calls[0].name, "onMoveFailure");
    }

    #[test]
    fn accelerate_to_point_cruises_then_stops_at_the_destination() {
        let mut app = app_with(&["onMove"]);
        let mut spaces = spaces_with();
        create(&mut app, &mut spaces, 77, [0.0, 0.0, 0.0]);

        // acceleration 100 clamps the desired velocity to maxSpeed the
        // whole way: half a metre a tick of cruise, then the landing
        // sequence bleeds the sampled speed off and fires onMove.
        app.attach_controller(
            77,
            Controller {
                kind: ControllerKind::accelerate_to_point(
                    [10.0, 0.0, 0.0],
                    100.0,
                    5.0,
                    AccelerationFacing::Velocity,
                    true,
                ),
                user_arg: 0,
            },
        );
        // The cruise holds while sqrt(2*d*a) >= maxSpeed, i.e. until
        // the destination is within 3.125 m — fourteen exact steps.
        for tick in 1..=14 {
            app.advance_tick();
            let entity = app.entity_population[&77].borrow();
            assert_eq!(entity.position, [0.5 * tick as f32, 0.0, 0.0]);
            assert_eq!(entity.direction[0], MATH_PI / 2.0);
            drop(entity);
        }
        // velocity() reads the sampled 5 m/s mid-cruise.
        assert_eq!(
            app.entity_population[&77].borrow().velocity,
            [5.0, 0.0, 0.0]
        );

        // The landing: the desired velocity decays with the remaining
        // distance, the position snaps onto the destination, and the
        // almost-stopped condition finally opens the gate.
        let mut arrived = false;
        let mut ticks_after_cruise = 0;
        for _ in 0..10 {
            app.advance_tick();
            ticks_after_cruise += 1;
            arrived = !app.entity_population[&77]
                .borrow()
                .controllers
                .container
                .contains_key(&MOVEMENT);
            if arrived {
                break;
            }
        }
        assert!(
            arrived,
            "no onMove within {ticks_after_cruise} ticks of the landing"
        );
        let entity = app.entity_population[&77].borrow();
        assert_eq!(entity.position, [10.0, 0.0, 0.0]);
        // The stop bleeds the velocity to almost-zero.
        assert!(
            Vector3::new(entity.velocity[0], entity.velocity[1], entity.velocity[2])
                .length_squared()
                < ALMOST_EQUAL_EPSILON
        );
        drop(entity);
        assert_eq!(app.script_calls.len(), 1);
        assert_eq!(app.script_calls[0].name, "onMove");

        app.advance_tick();
        assert_eq!(
            app.entity_population[&77].borrow().position,
            [10.0, 0.0, 0.0]
        );
        assert_eq!(app.script_calls.len(), 1);
    }

    #[test]
    fn accelerate_to_point_without_stop_arrives_at_speed() {
        let mut app = app_with(&["onMove"]);
        let mut spaces = spaces_with();
        create(&mut app, &mut spaces, 77, [0.0, 0.0, 0.0]);

        // Without stopAtDestination the arrival is a plain position
        // match: the controller finishes at full speed.
        app.attach_controller(
            77,
            Controller {
                kind: ControllerKind::accelerate_to_point(
                    [5.0, 0.0, 0.0],
                    100.0,
                    5.0,
                    AccelerationFacing::None,
                    false,
                ),
                user_arg: 0,
            },
        );
        for tick in 1..=9 {
            app.advance_tick();
            assert_eq!(
                app.entity_population[&77].borrow().position,
                [0.5 * tick as f32, 0.0, 0.0]
            );
        }
        app.advance_tick();
        let entity = app.entity_population[&77].borrow();
        assert_eq!(entity.position, [5.0, 0.0, 0.0]);
        assert_eq!(entity.velocity, [5.0, 0.0, 0.0]);
        assert!(entity.controllers.container.is_empty());
        drop(entity);
        assert_eq!(app.script_calls.len(), 1);
    }

    #[test]
    fn acceleration_faces_its_heading_per_the_facing_mode() {
        let mut app = app_with(&["onMove"]);
        let mut spaces = spaces_with();
        create(&mut app, &mut spaces, 77, [0.0, 0.0, 0.0]);
        app.entity_population[&77].borrow_mut().direction[0] = 0.25;

        // FACING_ACCELERATION turns along the acceleration vector from
        // the first tick.
        app.attach_controller(
            77,
            Controller {
                kind: ControllerKind::accelerate_to_point(
                    [10.0, 0.0, 0.0],
                    4.0,
                    5.0,
                    AccelerationFacing::Acceleration,
                    true,
                ),
                user_arg: 0,
            },
        );
        app.advance_tick();
        assert_eq!(
            app.entity_population[&77].borrow().direction[0],
            MATH_PI / 2.0
        );

        // FACING_NONE would have left the direction alone.
        let mut quiet = app_with(&["onMove"]);
        let mut quiet_spaces = spaces_with();
        create(&mut quiet, &mut quiet_spaces, 77, [0.0, 0.0, 0.0]);
        quiet.entity_population[&77].borrow_mut().direction[0] = 0.25;
        quiet.attach_controller(
            77,
            Controller {
                kind: ControllerKind::accelerate_to_point(
                    [10.0, 0.0, 0.0],
                    4.0,
                    5.0,
                    AccelerationFacing::None,
                    false,
                ),
                user_arg: 0,
            },
        );
        quiet.advance_tick();
        assert_eq!(quiet.entity_population[&77].borrow().direction[0], 0.25);
    }

    #[test]
    fn movement_pass_runs_before_the_timer_queue() {
        let mut app = app_with(&["onMove", "onTimer"]);
        let mut spaces = spaces_with();
        create(&mut app, &mut spaces, 77, [0.0, 0.0, 0.0]);

        // Both due on tick 1: ServerApp::advanceTime runs callUpdatables
        // ahead of the timer queue, so the movement's onMove lands
        // first.
        app.add_timer(77, 0.1, 0.0, 9);
        app.attach_controller(
            77,
            Controller {
                kind: ControllerKind::move_to_point([0.1, 0.0, 0.0], 1.0, true, false, 10),
                user_arg: 0,
            },
        );
        app.advance_tick();
        assert_eq!(app.script_calls.len(), 2);
        assert_eq!(app.script_calls[0].name, "onMove");
        assert_eq!(app.script_calls[1].name, "onTimer");
    }

    #[test]
    fn destroy_entity_leaves_no_updatable_behind() {
        let mut app = app_with(&["onMove"]);
        let mut spaces = spaces_with();
        create(&mut app, &mut spaces, 77, [0.0, 0.0, 0.0]);

        app.attach_controller(
            77,
            Controller {
                kind: ControllerKind::move_to_point([10.0, 0.0, 0.0], 5.0, true, false, 10),
                user_arg: 0,
            },
        );
        assert_eq!(app.updatable_controllers.len(), 1);
        app.destroy_entity(77);
        assert_eq!(app.updatable_controllers, Vec::new());
        assert!(app.entity_population[&77].borrow().destroyed);
    }

    #[test]
    fn navigation_drop_position_pushes_the_walk_below_the_clamp() {
        let mut app = app_with(&["onMove"]);
        let mut spaces = spaces_with();
        create(&mut app, &mut spaces, 77, [0.0, 0.0, 0.0]);

        app.should_navigation_drop_position = true;
        app.attach_controller(
            77,
            Controller {
                kind: ControllerKind::move_to_point([4.0, 0.0, 0.0], 4.0, false, false, 10),
                user_arg: 0,
            },
        );
        app.advance_tick();
        // 0.4 m of walk: the drop is climb(0.65) + step * gradient(1.0).
        let entity = app.entity_population[&77].borrow();
        assert_eq!(entity.position, [0.4, 0.65 + 0.4, 0.0]);
        drop(entity);
    }

    #[test]
    fn cancel_during_the_pass_leaves_a_hole_the_pass_skips() {
        let mut app = app_with(&["onMove", "onMoveFailure"]);
        let mut spaces = spaces_with();
        create(&mut app, &mut spaces, 77, [0.0, 0.0, 0.0]);
        create(&mut app, &mut spaces, 78, [10.0, 0.0, 0.0]);

        // A fails on its first update (missing target): its registry
        // slot becomes a hole mid-pass, and B — registered after it —
        // still steps this very tick.
        app.attach_controller(
            77,
            Controller {
                kind: ControllerKind::move_to_entity(999, 1.0, 5.0, true, false, 10),
                user_arg: 0,
            },
        );
        app.attach_controller(
            78,
            Controller {
                kind: ControllerKind::move_to_point([100.0, 0.0, 0.0], 10.0, true, false, 10),
                user_arg: 0,
            },
        );
        assert_eq!(
            app.updatable_controllers,
            vec![Some((77, MOVEMENT)), Some((78, MOVEMENT))]
        );
        app.advance_tick();
        // B walked its first metre despite A's mid-pass cancellation
        // (the category id is per-app, the registry slot per pair).
        assert_eq!(
            app.entity_population[&78].borrow().position,
            [11.0, 0.0, 0.0]
        );
        assert_eq!(app.updatable_controllers, vec![Some((78, MOVEMENT))]);
        // The compaction happens after the pass: a second tick runs with
        // a clean registry.
        app.advance_tick();
        assert_eq!(
            app.entity_population[&78].borrow().position,
            [12.0, 0.0, 0.0]
        );
    }

    #[test]
    fn exclusive_placeholders_hang_inert() {
        let mut app = app_with(&["onMove"]);
        let mut spaces = spaces_with();
        create(&mut app, &mut spaces, 77, [0.0, 0.0, 0.0]);

        // The pre-resolved exclusive face (BW-08-02-02's navigation
        // placeholder): attached but not in the per-tick registry, so
        // ticks leave it untouched.
        assert_eq!(app.get_exclusive_id(MOVEMENT_CATEGORY), 0x8000);
        let navigation = app.get_exclusive_id("Navigation");
        let id = app.attach_controller(
            77,
            Controller {
                kind: ControllerKind::Exclusive {
                    category: navigation,
                },
                user_arg: 0,
            },
        );
        assert_eq!(id, 0x8001);
        assert_eq!(app.updatable_controllers, Vec::new());
        app.advance_tick();
        app.advance_tick();
        assert_eq!(
            app.entity_population[&77].borrow().position,
            [0.0, 0.0, 0.0]
        );
        assert_eq!(
            app.entity_population[&77]
                .borrow()
                .controllers
                .container
                .len(),
            1
        );
    }

    #[test]
    fn movement_math_helpers_match_the_reference_faces() {
        // isZero is a bit-exact zero test, not an almostZero.
        assert!(is_zero(0.0));
        assert!(is_zero(-0.0));
        assert!(!is_zero(1e-30));

        // almostEqual's 0.0004 band, scalar and per component.
        assert!(almost_equal(1.0, 1.0 + 0.0003));
        assert!(!almost_equal(1.0, 1.0 + 2.0 * ALMOST_EQUAL_EPSILON));
        assert!(almost_equal_v3(
            Vector3::new(1.0, 2.0, 3.0),
            Vector3::new(1.0 + 0.0003, 2.0 - 0.0003, 3.0 + 0.0003)
        ));
        assert!(!almost_equal_v3(
            Vector3::new(1.0, 2.0, 3.0),
            Vector3::new(1.0, 2.0, 3.0 + 2.0 * ALMOST_EQUAL_EPSILON)
        ));

        // unitVector keeps a zero vector zero (the guarded normalise).
        assert_eq!(unit_vector(Vector3::ZERO), Vector3::ZERO);
        let unit = unit_vector(Vector3::new(3.0, 0.0, 4.0));
        assert_eq!(unit, Vector3::new(0.6, 0.0, 0.8));

        // projectOnto: src on dest's line.
        let projected = project_onto(Vector3::new(2.0, 1.0, 0.0), Vector3::new(1.0, 0.0, 0.0));
        assert_eq!(projected, Vector3::new(2.0, 0.0, 0.0));

        // Per-component division, exact against `newVelocity / hertz`.
        assert_eq!(
            divided_by(Vector3::new(1.0, -2.0, 4.0), 2.0),
            Vector3::new(0.5, -1.0, 2.0)
        );

        // turnRange/isBetween: the positive-direction arc faces.
        assert_eq!(turn_range(0.0, 1.0), 1.0);
        assert!((turn_range(1.0, 0.0) - (MATH_PI * 2.0 - 1.0)).abs() < 1e-6);
        assert!(is_between(0.5, 0.0, 1.0));
        assert!(!is_between(2.0, 0.0, 1.0));
        // An arc that wraps through π.
        assert!(is_between(-3.0, 3.0, -3.0));
        assert!(!is_between(0.0, 3.0, -3.0));

        // calculateDesiredVelocity: cruise clamps to maxSpeed; the stop
        // face bleeds with sqrt(2·d·a).
        let cruise = calculate_desired_velocity(
            Vector3::ZERO,
            Vector3::new(10.0, 0.0, 0.0),
            4.0,
            2.0,
            false,
        );
        assert_eq!(cruise, Vector3::new(2.0, 0.0, 0.0));
        let stop = calculate_desired_velocity(
            Vector3::ZERO,
            Vector3::new(10.0, 0.0, 0.0),
            4.0,
            100.0,
            true,
        );
        assert!((stop.length() - (2.0 * 10.0f32 * 4.0).sqrt()).abs() < 1e-5);

        // calculateAccelerationVector: equal velocities face the desired
        // vector; a near-zero desired steers straight against the
        // current one; a resting current takes the desired straight.
        assert_eq!(
            calculate_acceleration_vector(Vector3::new(2.0, 0.0, 0.0), Vector3::new(2.0, 0.0, 0.0)),
            Vector3::new(1.0, 0.0, 0.0)
        );
        assert_eq!(
            calculate_acceleration_vector(Vector3::new(1.0, 0.0, 0.0), Vector3::ZERO),
            Vector3::new(-1.0, 0.0, 0.0)
        );
        assert_eq!(
            calculate_acceleration_vector(Vector3::ZERO, Vector3::new(0.0, 3.0, 0.0)),
            Vector3::new(0.0, 1.0, 0.0)
        );
        // A correction case: the acceleration points along difference +
        // twice its off-desired remainder, normalised.
        let corrected =
            calculate_acceleration_vector(Vector3::new(1.0, 0.0, 0.0), Vector3::new(2.0, 2.0, 0.0));
        assert!((corrected.length() - 1.0).abs() < 1e-6);

        // facing_vector_of: the three modes.
        let acceleration = Vector3::new(1.0, 0.0, 0.0);
        let moving = Vector3::new(0.0, 2.0, 0.0);
        assert_eq!(
            facing_vector_of(
                AccelerationFacing::Velocity,
                moving,
                Vector3::ZERO,
                acceleration,
                0.4
            ),
            Vector3::new(0.0, 1.0, 0.0)
        );
        // A standing velocity faces nothing.
        assert_eq!(
            facing_vector_of(
                AccelerationFacing::Velocity,
                Vector3::ZERO,
                Vector3::ZERO,
                acceleration,
                0.4
            ),
            Vector3::ZERO
        );
        // Acceleration faces the acceleration while correcting and the
        // new velocity once it matches.
        assert_eq!(
            facing_vector_of(
                AccelerationFacing::Acceleration,
                moving,
                Vector3::new(2.0, 0.0, 0.0),
                acceleration,
                0.4
            ),
            acceleration
        );
        assert_eq!(
            facing_vector_of(
                AccelerationFacing::Acceleration,
                moving,
                Vector3::new(0.1, 0.0, 0.0),
                acceleration,
                0.4
            ),
            Vector3::new(0.0, 1.0, 0.0)
        );
        // None faces nothing.
        assert_eq!(
            facing_vector_of(
                AccelerationFacing::None,
                moving,
                Vector3::new(2.0, 0.0, 0.0),
                acceleration,
                0.4
            ),
            Vector3::ZERO
        );
    }
    #[test]
    fn a_slot_nulled_mid_pass_is_skipped() {
        let mut app = app_with(&["onMove"]);
        let mut spaces = spaces_with();
        create(&mut app, &mut spaces, 77, [0.0, 0.0, 0.0]);
        create(&mut app, &mut spaces, 78, [0.0, 0.0, 0.0]);

        // A arrives on its very first step; B would walk but is
        // cancelled before the pass reaches it. Replicating the pass's
        // iteration window (`Updatables::call` runs with its flag set,
        // visiting each index of the initial size): A's slot is holed
        // while it is visited, B's afterwards — and the visit that
        // lands on B's nulled slot skips it.
        app.attach_controller(
            77,
            Controller {
                kind: ControllerKind::move_to_point([0.1, 0.0, 0.0], 5.0, true, false, 10),
                user_arg: 0,
            },
        );
        app.attach_controller(
            78,
            Controller {
                kind: ControllerKind::move_to_point([100.0, 0.0, 0.0], 10.0, true, false, 10),
                user_arg: 0,
            },
        );
        assert_eq!(app.script_calls.len(), 0);
        app.in_movement_update = true;
        app.update_registered_controller(0);
        assert_eq!(app.updatable_controllers, vec![None, Some((78, MOVEMENT))]);
        app.cancel_controller(78, MOVEMENT);
        assert_eq!(app.updatable_controllers, vec![None, None]);
        app.update_registered_controller(1);
        assert_eq!(
            app.entity_population[&78].borrow().position,
            [0.0, 0.0, 0.0]
        );
        app.in_movement_update = false;
        // The post-pass compaction clears the holes.
        app.advance_tick();
        assert_eq!(app.updatable_controllers, Vec::new());
        assert_eq!(app.script_calls.len(), 1);
        assert_eq!(app.script_calls[0].name, "onMove");
    }

    #[test]
    fn move_towards_spends_the_remaining_ticks_budget() {
        let mut app = app_with(&[]);
        let mut spaces = spaces_with();
        create(&mut app, &mut spaces, 77, [0.0, 0.0, 0.0]);
        let entity_rc = app.entity_population[&77].clone();

        // Walking: the budget buys `remaining * metresPerTick` metres
        // and is spent to zero — three ticks' worth, one and a half
        // metres in a single step.
        let mut remaining = 3.0_f32;
        let arrived = app.move_towards(
            &entity_rc,
            0.5,
            false,
            false,
            Vector3::new(10.0, 0.0, 0.0),
            Some(&mut remaining),
        );
        assert!(!arrived);
        assert_eq!(
            app.entity_population[&77].borrow().position,
            [1.5, 0.0, 0.0]
        );
        assert_eq!(remaining, 0.0);

        // Arrival: the landing scales the budget by the un-walked
        // fraction — two ticks' worth of metres against a 0.9 m hop
        // leaves 0.4 of a tick.
        app.set_entity_position_and_direction(
            &entity_rc,
            Vector3::new(0.0, 0.0, 0.0),
            Vector3::new(0.0, 0.0, 0.0),
        );
        let mut remaining = 2.0_f32;
        let arrived = app.move_towards(
            &entity_rc,
            0.5,
            false,
            false,
            Vector3::new(0.9, 0.0, 0.0),
            Some(&mut remaining),
        );
        assert!(arrived);
        assert_eq!(
            app.entity_population[&77].borrow().position,
            [0.9, 0.0, 0.0]
        );
        assert!((remaining - 0.4).abs() < 1e-6);
    }

    #[test]
    fn move_to_entity_vertical_close_stops_without_facing() {
        let mut app = app_with(&["onMove"]);
        let mut spaces = spaces_with();
        create(&mut app, &mut spaces, 77, [0.0, 0.0, 0.0]);
        create(&mut app, &mut spaces, 78, [0.0, 0.5, 0.0]);
        app.entity_population[&77].borrow_mut().direction[0] = 0.25;

        // moveVertically keeps the height difference in the close-enough
        // distance, and faceMovement off leaves the yaw alone.
        app.attach_controller(
            77,
            Controller {
                kind: ControllerKind::move_to_entity(78, 1.0, 5.0, false, true, 10),
                user_arg: 0,
            },
        );
        app.advance_tick();
        let entity = app.entity_population[&77].borrow();
        assert_eq!(entity.position, [0.0, 0.0, 0.0]);
        assert_eq!(entity.direction[0], 0.25);
        assert!(entity.controllers.container.is_empty());
        drop(entity);
        assert_eq!(app.script_calls.len(), 1);
        assert_eq!(app.script_calls[0].name, "onMove");
    }

    #[test]
    fn accelerate_lands_verbatim_on_a_three_d_short_hop() {
        let mut app = app_with(&["onMove"]);
        let mut spaces = spaces_with();
        create(&mut app, &mut spaces, 77, [0.0, 0.0, 0.0]);

        // The first step overshoots the hop outright: the desired speed
        // sqrt(2*d*a) is ~3.28 m/s, half a tick's worth of travel is
        // already past the destination, so the landing copies it
        // verbatim — including the height — and faces the travel vector
        // in full (yaw and pitch).
        app.attach_controller(
            77,
            Controller {
                kind: ControllerKind::accelerate_to_point(
                    [0.02, 0.05, 0.0],
                    100.0,
                    5.0,
                    AccelerationFacing::Velocity,
                    true,
                ),
                user_arg: 0,
            },
        );
        app.advance_tick();
        let entity = app.entity_population[&77].borrow();
        assert_eq!(entity.position, [0.02, 0.05, 0.0]);
        // The travel vector is +x with +y: yaw atan2(x, z) is a right
        // angle, pitch -atan2(0.05, 0.02).
        assert_eq!(entity.direction[0], MATH_PI / 2.0);
        assert!((entity.direction[1] - normalise_angle(-0.05_f32.atan2(0.02))).abs() < 1e-6);
        assert!(entity.controllers.container.is_empty());
        drop(entity);
        assert_eq!(app.script_calls.len(), 1);
        assert_eq!(app.script_calls[0].name, "onMove");
    }

    #[test]
    fn accelerate_clamp_normalises_an_over_length_desired_velocity() {
        let mut app = app_with(&[]);
        let mut spaces = spaces_with();
        create(&mut app, &mut spaces, 77, [0.0, 0.0, 0.0]);
        let entity_rc = app.entity_population[&77].clone();

        // The reference re-clamps the desired velocity after
        // calculateDesiredVelocity: a negative maxSpeed degenerately
        // produces one over the cap (and pointing away from the
        // destination) — the clamp normalises it back to the cap on the
        // destination line, and the entity walks toward it.
        let arrived = app.accelerate_move(
            &entity_rc,
            10.0,
            -2.0,
            AccelerationFacing::None,
            false,
            [10.0, 0.0, 0.0],
        );
        assert!(!arrived);
        assert_eq!(
            app.entity_population[&77].borrow().position,
            [0.1, 0.0, 0.0]
        );
        assert_eq!(
            app.entity_population[&77].borrow().direction,
            [0.0, 0.0, 0.0]
        );
    }

    #[test]
    fn the_pass_tolerates_stale_registry_entries() {
        let mut app = app_with(&["onTimer", "onMove"]);
        let mut spaces = spaces_with();
        create(&mut app, &mut spaces, 77, [0.0, 0.0, 0.0]);
        let entity_rc = app.entity_population[&77].clone();

        // An entity gone from the population, an id gone from its
        // controller map, a restart aimed at a cancelled id, and a
        // timer-style start aimed at a movement controller: each is a
        // desync the registry guards against, not a step.
        app.update_movement_controller(999, MOVEMENT);
        app.update_movement_controller(77, 0x1997);
        app.start_real_controller(&entity_rc, 0x1997, true);
        app.attach_controller(
            77,
            Controller {
                kind: ControllerKind::move_to_point([10.0, 0.0, 0.0], 5.0, true, false, 10),
                user_arg: 0,
            },
        );
        app.start_timer_controller(&entity_rc, MOVEMENT, true);
        assert_eq!(
            app.entity_population[&77].borrow().position,
            [0.0, 0.0, 0.0]
        );
        assert_eq!(app.script_calls.len(), 0);
        // The walker goes away before the ticks below.
        app.cancel_controller(77, MOVEMENT);

        // And an entry whose id resolves to a non-movement family: the
        // dispatch runs its inert arm (timers and exclusive placeholders
        // only ever get here by desync) and leaves the entity alone.
        let navigation = app.get_exclusive_id("Navigation");
        let inert = app.attach_controller(
            77,
            Controller {
                kind: ControllerKind::Exclusive {
                    category: navigation,
                },
                user_arg: 0,
            },
        );
        app.updatable_controllers.push(Some((77, inert)));
        app.advance_tick();
        app.advance_tick();
        assert_eq!(
            app.entity_population[&77].borrow().position,
            [0.0, 0.0, 0.0]
        );
        assert_eq!(app.script_calls.len(), 0);
    }

    #[test]
    fn a_finished_callback_that_lost_the_controller_skips_the_cancel() {
        let mut app = app_with(&["onMoveFailure", "onMove"]);
        let mut spaces = spaces_with();
        create(&mut app, &mut spaces, 77, [0.0, 0.0, 0.0]);
        create(&mut app, &mut spaces, 78, [0.0, 0.0, 0.0]);
        let walker = app.entity_population[&77].clone();
        let other = app.entity_population[&78].clone();

        // The reference holds a ControllerPtr across the standard
        // callback, so a script that cancels inside the callback leaves
        // the post-callback self-cancel a no-op. The port reads that as
        // "the id is gone from the map": the failure branch and the
        // finish branch each skip their cancel.
        app.update_move_to_entity(
            &walker,
            77,
            0x1997,
            999,
            1.0,
            5.0,
            true,
            false,
            [0.0, 0.0],
            true,
        );
        app.finish_movement_callback(&other, 78, 0x1997, "onMove");
        assert_eq!(app.script_calls.len(), 2);
        assert_eq!(app.script_calls[0].name, "onMoveFailure");
        assert_eq!(app.script_calls[1].name, "onMove");
        assert!(
            app.entity_population[&77]
                .borrow()
                .controllers
                .container
                .is_empty()
        );
        assert!(
            app.entity_population[&78]
                .borrow()
                .controllers
                .container
                .is_empty()
        );
    }
}
