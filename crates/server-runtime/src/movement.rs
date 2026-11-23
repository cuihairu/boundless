//! BW-08-02-03: port the movement, turn and acceleration controls.
//!
//! `BaseAccelerationController::move` (`base_acceleration_controller.cpp:173`)
//! advances the entity one tick toward a destination: it computes a desired
//! velocity (`calculateDesiredVelocity`, `:309`), picks the acceleration
//! direction (`calculateAccelerationVector`, `:349`), steps the velocity by
//! `acceleration / updateHertz` (`accelerationPerTick`, `:202`) toward the
//! desired one, and advances the position by `newVelocity / updateHertz`
//! (`:227`), arriving exactly when the step would overshoot. The desired
//! velocity is `maxSpeed` ahead, or `sqrt(2 * distance * acceleration)` when
//! stopping at the destination — the speed reached from zero over the
//! remaining distance. Facing follows the velocity or the acceleration vector
//! (`yaw() = atan2(x, z)`, `vector3.ipp:548`).
//!
//! `YawRotatorController` (`turn_controller.cpp`) turns the entity's yaw
//! toward a target: `radiansPerTick = velocity / updateHertz` (`:89`), the
//! shortest direction is chosen on start (`startReal`, `:157`), and each tick
//! the yaw moves by `radiansPerTick`, clamping onto the target when it passes
//! (`turn`, `:172`). [`Angle::isBetween`] decides the arcs.
//!
//! The acceptance, "固定输入轨迹吻合", is a fixed input run: the per-tick
//! positions and yaws must follow these exact recurrences.

/// The epsilon used by `almostEqual` / `almostZero` (`stdmf.hpp:606`).
const EPSILON: f64 = 0.0004;

/// `CellAppConfig::updateHertz()` — the default tick rate.
pub const UPDATE_HERTZ: f64 = 20.0;

/// `Vector3` (`vector3.hpp`): a 3D vector.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Vec3 {
    pub x: f64,
    pub y: f64,
    pub z: f64,
}

impl Vec3 {
    /// The origin.
    pub const ZERO: Vec3 = Vec3::new(0.0, 0.0, 0.0);

    /// A vector (`Vector3(x, y, z)`).
    pub const fn new(x: f64, y: f64, z: f64) -> Self {
        Vec3 { x, y, z }
    }

    /// `length()`.
    pub fn length(self) -> f64 {
        self.length_squared().sqrt()
    }

    /// `lengthSquared()`.
    pub fn length_squared(self) -> f64 {
        self.x * self.x + self.y * self.y + self.z * self.z
    }

    /// `unitVector()`: a unit-length copy, zero for a zero vector.
    pub fn unit(self) -> Vec3 {
        let len = self.length();
        if len == 0.0 {
            return Vec3::ZERO;
        }
        self.scale(1.0 / len)
    }

    /// `dotProduct`.
    pub fn dot(self, other: Vec3) -> f64 {
        self.x * other.x + self.y * other.y + self.z * other.z
    }

    /// Vector addition (`operator+`).
    pub fn plus(self, other: Vec3) -> Vec3 {
        self + other
    }

    /// Vector subtraction (`operator-`).
    pub fn minus(self, other: Vec3) -> Vec3 {
        self - other
    }

    /// Scalar multiplication.
    pub fn scale(self, s: f64) -> Vec3 {
        Vec3::new(self.x * s, self.y * s, self.z * s)
    }

    /// `yaw()` (`vector3.ipp:548`): `atan2(x, z)`.
    pub fn yaw(self) -> f64 {
        self.x.atan2(self.z)
    }

    /// `projectOnto(v1, v2)` (`vector3.ipp:185`): this vector projected onto
    /// the line of `dir`.
    pub fn project_onto(self, dir: Vec3) -> Vec3 {
        dir.scale(self.dot(dir) / dir.length_squared())
    }
}

impl std::ops::Add for Vec3 {
    type Output = Vec3;

    /// `operator+`.
    fn add(self, other: Vec3) -> Vec3 {
        Vec3::new(self.x + other.x, self.y + other.y, self.z + other.z)
    }
}

impl std::ops::Sub for Vec3 {
    type Output = Vec3;

    /// `operator-`.
    fn sub(self, other: Vec3) -> Vec3 {
        Vec3::new(self.x - other.x, self.y - other.y, self.z - other.z)
    }
}

/// `almostEqual` for two vectors (`stdmf.hpp:627`): component-wise within
/// `EPSILON`.
fn almost_equal_vec(a: Vec3, b: Vec3) -> bool {
    (a.x - b.x).abs() < EPSILON && (a.y - b.y).abs() < EPSILON && (a.z - b.z).abs() < EPSILON
}

/// `almostZero` (`stdmf.hpp:616`).
fn almost_zero(x: f64) -> bool {
    -EPSILON < x && x < EPSILON
}

/// `FACING_NONE` / `FACING_VELOCITY` / `FACING_ACCELERATION`
/// (`base_acceleration_controller.hpp`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Facing {
    /// Do not change the facing.
    None,
    /// Face the direction of travel (`FACING_VELOCITY`).
    Velocity,
    /// Face the direction of the acceleration (`FACING_ACCELERATION`).
    Acceleration,
}

/// `BaseAccelerationController` (`base_acceleration_controller.hpp`): moves
/// the entity toward a destination, accelerating to a maximum speed and
/// optionally stopping there.
#[derive(Debug, Clone)]
pub struct MovementController {
    /// `acceleration_`: acceleration in metres/s².
    acceleration: f64,
    /// `maxSpeed_`.
    max_speed: f64,
    /// `facing_`.
    facing: Facing,
    /// `stopAtDestination`.
    stop_at_destination: bool,
    /// The destination (`destination_`).
    destination: Vec3,
    /// `CellAppConfig::updateHertz()`.
    hertz: f64,
    /// `localPosition_`.
    position: Vec3,
    /// `localDirection_.yaw`.
    yaw: f64,
    /// `currVelocity_`.
    current_velocity: Vec3,
    /// Whether the entity has reached the destination.
    arrived: bool,
}

impl MovementController {
    /// A controller starting at the origin with zero velocity.
    pub fn new(
        acceleration: f64,
        max_speed: f64,
        facing: Facing,
        stop_at_destination: bool,
        destination: Vec3,
        hertz: f64,
    ) -> Self {
        MovementController {
            acceleration,
            max_speed,
            facing,
            stop_at_destination,
            destination,
            hertz,
            position: Vec3::ZERO,
            yaw: 0.0,
            current_velocity: Vec3::ZERO,
            arrived: false,
        }
    }

    /// A controller restored from a stream (`readRealFromStream`,
    /// `base_acceleration_controller.cpp:405`): start at a position and
    /// velocity.
    #[allow(clippy::too_many_arguments)]
    pub fn with_state(
        acceleration: f64,
        max_speed: f64,
        facing: Facing,
        stop_at_destination: bool,
        destination: Vec3,
        position: Vec3,
        yaw: f64,
        current_velocity: Vec3,
        hertz: f64,
    ) -> Self {
        MovementController {
            acceleration,
            max_speed,
            facing,
            stop_at_destination,
            destination,
            hertz,
            position,
            yaw,
            current_velocity,
            arrived: false,
        }
    }

    /// `calculateDesiredVelocity` (`base_acceleration_controller.cpp:309`).
    fn desired_velocity(&self) -> Vec3 {
        let destination_vector = self.destination.minus(self.position);
        let mut desired = destination_vector.unit();
        if self.stop_at_destination {
            // The speed reached by accelerating from zero over the remaining
            // distance.
            let speed_at_distance = (2.0 * destination_vector.length() * self.acceleration).sqrt();
            desired = desired.scale(self.max_speed.min(speed_at_distance));
        } else {
            desired = desired.scale(self.max_speed);
        }
        desired
    }

    /// `calculateAccelerationVector` (`base_acceleration_controller.cpp:349`):
    /// the direction to accelerate to move the current velocity toward the
    /// desired one.
    fn acceleration_vector(&self, current: Vec3, desired: Vec3) -> Vec3 {
        if almost_equal_vec(current, desired) {
            return desired.unit();
        }
        if almost_zero(desired.length_squared()) {
            return current.unit().scale(-1.0);
        }
        if current.length_squared() > 0.0 {
            let velocity_difference = desired.minus(current);
            let mut accel = velocity_difference.project_onto(desired);
            accel = accel.plus(velocity_difference.minus(accel).scale(2.0));
            accel.unit()
        } else {
            desired.unit()
        }
    }

    /// `BaseAccelerationController::move` (`:173`): advance one tick.
    pub fn update(&mut self) {
        if self.arrived {
            return;
        }

        let position = self.position;
        let destination_vector = self.destination.minus(position);
        let current_velocity = self.current_velocity;
        let desired_velocity = self.desired_velocity();
        let acceleration_vector = self.acceleration_vector(current_velocity, desired_velocity);
        let velocity_difference = current_velocity.minus(desired_velocity);
        let acceleration_per_tick = self.acceleration / self.hertz;

        let new_velocity = if velocity_difference.length() > acceleration_per_tick {
            current_velocity.plus(acceleration_vector.scale(acceleration_per_tick))
        } else {
            desired_velocity
        };

        let step_distance = new_velocity.length() / self.hertz;
        let position = if step_distance > destination_vector.length() {
            self.arrived = true;
            self.destination
        } else {
            position.plus(new_velocity.scale(1.0 / self.hertz))
        };

        // Face the direction of travel or the acceleration.
        if self.facing != Facing::None {
            let mut facing_vector = Vec3::ZERO;
            if self.facing == Facing::Velocity && new_velocity.length_squared() > 0.0 {
                facing_vector = new_velocity.unit();
            } else if self.facing == Facing::Acceleration {
                if velocity_difference.length() > acceleration_per_tick {
                    facing_vector = acceleration_vector;
                } else {
                    facing_vector = new_velocity.unit();
                }
            }
            if facing_vector.x != 0.0 || facing_vector.z != 0.0 {
                self.yaw = facing_vector.yaw();
            }
        }

        self.position = position;
        self.current_velocity = new_velocity;
    }

    /// The current local position.
    pub fn position(&self) -> Vec3 {
        self.position
    }

    /// The current yaw.
    pub fn yaw(&self) -> f64 {
        self.yaw
    }

    /// The current velocity.
    pub fn velocity(&self) -> Vec3 {
        self.current_velocity
    }

    /// Whether the destination has been reached.
    pub fn arrived(&self) -> bool {
        self.arrived
    }
}

/// `Angle::normalise` (`angle.hpp:52`): wrap into `(-π, π]`.
fn normalise_angle(a: f64) -> f64 {
    let two_pi = 2.0 * std::f64::consts::PI;
    let mut r = a.rem_euclid(two_pi);
    if r > std::f64::consts::PI {
        r -= two_pi;
    }
    r
}

/// `Angle::normalise` into `[0, 2π)`.
fn normalise_tau(a: f64) -> f64 {
    a.rem_euclid(2.0 * std::f64::consts::PI)
}

/// `Angle::isBetween(min, max)` (`angle.hpp:66`): whether `angle` lies on the
/// clockwise arc from `min` to `max`.
fn is_between(angle: f64, min: f64, max: f64) -> bool {
    normalise_tau(angle - min) <= normalise_tau(max - min)
}

/// `YawRotatorController` (`turn_controller.hpp`): turn an entity's yaw toward
/// a target at a fixed angular rate.
#[derive(Debug, Clone)]
pub struct YawRotator {
    /// `targetYaw_`.
    target_yaw: f64,
    /// `radiansPerTick_`.
    radians_per_tick: f64,
    /// The current yaw.
    yaw: f64,
    /// Whether the target yaw has been reached.
    reached: bool,
    /// Whether `startReal` ran (the first turn already happened).
    started: bool,
}

impl YawRotator {
    /// `YawRotatorController::YawRotatorController(targetYaw, velocity)`
    /// (`turn_controller.cpp:73`): `radiansPerTick = velocity / hertz`.
    pub fn new(target_yaw: f64, velocity: f64, hertz: f64) -> Self {
        YawRotator {
            target_yaw: normalise_angle(target_yaw),
            radians_per_tick: velocity / hertz,
            yaw: 0.0,
            reached: false,
            started: false,
        }
    }

    /// `YawRotatorController::startReal` (`turn_controller.cpp:157`): choose
    /// the shortest direction and turn once.
    pub fn start(&mut self, current_yaw: f64) {
        self.yaw = normalise_angle(current_yaw);
        // Turn anti-clockwise when the target is on that side.
        if is_between(self.target_yaw, self.yaw - std::f64::consts::PI, self.yaw) {
            self.radians_per_tick = -self.radians_per_tick;
        }
        self.started = true;
        self.turn();
    }

    /// `YawRotatorController::turn` (`turn_controller.cpp:172`): advance the
    /// yaw one step toward the target. Returns whether the target was reached
    /// (the reference reports it via the return value; `update` records it).
    pub fn turn(&mut self) -> bool {
        let current_yaw = self.yaw;
        let mut new_yaw = normalise_angle(current_yaw + self.radians_per_tick);
        let reached = if self.radians_per_tick > 0.0 {
            is_between(self.target_yaw, current_yaw, new_yaw)
        } else {
            is_between(self.target_yaw, new_yaw, current_yaw)
        };
        if reached {
            new_yaw = self.target_yaw;
        }
        self.yaw = new_yaw;
        reached
    }

    /// `update` (`turn_controller.cpp:201`): one tick. Records and reports
    /// whether the target was reached.
    pub fn update(&mut self) -> bool {
        if self.reached {
            return true;
        }
        let reached = self.turn();
        if reached {
            self.reached = true;
        }
        reached
    }

    /// The current yaw.
    pub fn yaw(&self) -> f64 {
        self.yaw
    }

    /// Whether the target has been reached.
    pub fn reached(&self) -> bool {
        self.reached
    }

    /// Whether `startReal` ran.
    pub fn started(&self) -> bool {
        self.started
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-6
    }

    /// A straight run with no deceleration: velocity ramps by
    /// `acceleration / hertz` each tick up to `maxSpeed`, and the position
    /// advances by `velocity / hertz` (`base_acceleration_controller.cpp:202`,
    /// `:227`).
    #[test]
    fn a_straight_run_ramps_velocity_and_advances_position() {
        let hertz = 20.0;
        let mut controller = MovementController::new(
            10.0,
            5.0,
            Facing::None,
            false,
            Vec3::new(1000.0, 0.0, 0.0),
            hertz,
        );
        let accel_per_tick = 10.0 / hertz; // 0.5

        controller.update();
        assert!(close(controller.velocity().x, accel_per_tick));
        assert!(close(controller.position().x, accel_per_tick / hertz));
        assert_eq!(controller.position().y, 0.0);

        controller.update();
        assert!(close(controller.velocity().x, 2.0 * accel_per_tick));
        assert!(close(
            controller.position().x,
            (accel_per_tick + 2.0 * accel_per_tick) / hertz
        ));

        // Reaches max speed after 10 ticks and holds it.
        for _ in 0..8 {
            controller.update();
        }
        assert!(close(controller.velocity().x, 5.0), "clamped at max speed");
        assert!(!controller.arrived());

        // Steady state: one tick moves 5/20 = 0.25.
        let before = controller.position().x;
        controller.update();
        assert!(close(controller.position().x - before, 5.0 / hertz));
    }

    /// Stopping at the destination decelerates to `sqrt(2 * d * a)` and lands
    /// exactly on the destination.
    #[test]
    fn stopping_at_the_destination_lands_exactly() {
        let hertz = 20.0;
        let destination = Vec3::new(10.0, 0.0, 0.0);
        let mut controller = MovementController::new(
            5.0,
            100.0, // max speed never reached: the deceleration ramp dominates
            Facing::None,
            true,
            destination,
            hertz,
        );

        let mut ticks = 0;
        while !controller.arrived() && ticks < 10_000 {
            controller.update();
            ticks += 1;
        }
        assert!(controller.arrived());
        assert_eq!(controller.position(), destination, "lands exactly");
        assert_eq!(controller.position().x, 10.0);
        assert!(ticks > 0);
    }

    /// Facing the velocity turns the yaw to the direction of travel
    /// (`facing_ == FACING_VELOCITY`, `base_acceleration_controller.cpp:237`).
    #[test]
    fn facing_velocity_aims_the_direction_of_travel() {
        let hertz = 20.0;
        // Move diagonally: destination (10, 0, 10). yaw = atan2(x, z) = π/4.
        let mut controller = MovementController::new(
            2.0,
            1.0,
            Facing::Velocity,
            false,
            Vec3::new(10.0, 0.0, 10.0),
            hertz,
        );
        controller.update();
        assert!(close(controller.yaw(), std::f64::consts::FRAC_PI_4));
    }

    /// The yaw rotator turns a fixed `radiansPerTick` toward the target,
    /// choosing the shortest direction, and clamps onto the target.
    #[test]
    fn the_yaw_rotator_turns_by_a_fixed_rate_toward_the_target() {
        let hertz = 20.0;
        // From 90° to 0° at π/2 rad/s: π/40 per tick, the shortest direction
        // is anticlockwise.
        let mut rotator = YawRotator::new(0.0, std::f64::consts::FRAC_PI_2, hertz);
        // `startReal` turns once already (`turn_controller.cpp:166`).
        rotator.start(std::f64::consts::FRAC_PI_2);
        assert!(rotator.radians_per_tick < 0.0, "turns anticlockwise");
        assert!(
            close(
                rotator.yaw(),
                std::f64::consts::FRAC_PI_2 - std::f64::consts::PI / 40.0
            ),
            "the first turn happened in start"
        );

        // Each update turns one more π/40, until the 20th turn reaches 0.
        for turn in 2..=19 {
            rotator.update();
            assert!(!rotator.reached(), "turn {turn}");
            let expected = std::f64::consts::FRAC_PI_2 - turn as f64 * std::f64::consts::PI / 40.0;
            assert!(close(rotator.yaw(), expected), "turn {turn}");
        }
        rotator.update();
        assert!(rotator.reached());
        assert!(close(rotator.yaw(), 0.0), "clamped onto the target");
    }

    /// The yaw rotator turns clockwise when that is shorter.
    #[test]
    fn the_yaw_rotator_turns_the_shortest_way() {
        let hertz = 20.0;
        // From 0° to 90°: the shortest way is clockwise (increasing yaw).
        let mut rotator = YawRotator::new(
            std::f64::consts::FRAC_PI_2,
            std::f64::consts::FRAC_PI_2,
            hertz,
        );
        rotator.start(0.0);
        assert!(rotator.radians_per_tick > 0.0, "turns clockwise");
        assert!(
            close(rotator.yaw(), std::f64::consts::PI / 40.0),
            "the first turn happened in start"
        );
    }

    /// The fixed-input trajectory of a full run: position advances by the
    /// velocity recurrence each tick, matching the reference exactly.
    #[test]
    fn the_fixed_input_trajectory_matches() {
        let hertz = 10.0;
        let mut controller = MovementController::with_state(
            4.0,
            2.0,
            Facing::None,
            false,
            Vec3::new(100.0, 0.0, 0.0),
            Vec3::ZERO,
            0.0,
            Vec3::ZERO,
            hertz,
        );

        let mut positions = Vec::new();
        for _ in 0..6 {
            controller.update();
            positions.push(controller.position().x);
        }
        // accelPerTick = 4/10 = 0.4; velocity ramps 0.4, 0.8, 1.2, 1.6, 2.0,
        // 2.0 (clamped); position is the cumulative velocity/hertz.
        let expected = [0.04, 0.12, 0.24, 0.40, 0.60, 0.80];
        for (got, want) in positions.iter().zip(expected.iter()) {
            assert!(close(*got, *want), "got {got}, want {want}");
        }
        assert!(close(controller.velocity().x, 2.0));
    }

    /// `projectOnto` matches `v2 * (v1.dot(v2) / v2.lengthSquared())`.
    #[test]
    fn projection_onto_a_line() {
        let v = Vec3::new(3.0, 4.0, 0.0);
        let dir = Vec3::new(1.0, 0.0, 0.0);
        let projected = v.project_onto(dir);
        assert!(close(projected.x, 3.0));
        assert!(close(projected.y, 0.0));
    }
}
