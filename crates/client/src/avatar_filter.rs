//! Server prediction correction (BW-11-02-05).
//!
//! The reference corrects a client-side predicted entity position with
//! `AvatarFilterHelper` (`lib/connection/avatar_filter_helper.cpp`), the
//! interpolation/correction engine behind `AvatarFilter`. It keeps the last
//! `NUM_STORED_INPUTS` (8, `:16`) best-effort server samples in a ring
//! buffer, adapts a rendering latency toward the ideal (the age of the
//! sample the output should follow), and emits the position at
//! `time - latency` interpolated between two waypoints chosen from the
//! samples — so the rendered entity converges to the server's
//! authoritative samples instead of popping.
//!
//! `input` (`:299-335`) resets the buffer on the first sample
//! (`resetStoredInputs`, `:444-486`, which seeds the ring with the reset
//! sample and the waypoints `NONZERO_TIME_DIFFERENCE` (0.01s) apart),
//! discards a stale sample (`time <= newest`, `:313-316`), and pushes the
//! sample into the ring (the newest is index 0,
//! `getStoredInput(i)` = the sample `i` back, `:745-748`).
//!
//! `output` (`:355-414`) recomputes the ideal latency when a new sample
//! arrived — the time offset of the sample `s_latencyFrames_` (2.0) frames
//! back, clamped to `s_latencyMinimum_` (0.10s, `:30-47`) — then moves the
//! latency toward it at `s_latencyVelocity_` (1.0) scaled by the squared
//! error (`s_latencyCurvePower_` 2.0), and calls `extract` at
//! `time - latency`.
//!
//! `extract` (`:507-556`) interpolates between the waypoints: an inactive
//! filter emits the newest sample verbatim with zero velocity (`:513-524`);
//! otherwise it advances the waypoints past `time` with
//! `chooseNextWaypoint` (`:526-529`), lerps the position
//! (`Vector3::lerp`), the velocity (the waypoint slope), and the yaw and
//! pitch (`Angle::lerp`, the shortest arc, `angle.ipp:264-267`), with roll
//! always 0 (`:555`).
//!
//! `chooseNextWaypoint` (`:586-733`) is the correction. The next waypoint
//! is the oldest stored input still ahead of `time` (the scan at
//! `:602-610`), the look-ahead is the newest input; the extrapolated
//! look-ahead position is clamped to the newest input's error box
//! (`:649-651`); when only the error box changed and the boxes overlap the
//! position is held still (`:674-686`); otherwise the waypoint is
//! interpolated toward the look-ahead (`:689-696`) and constrained to its
//! input's error box by projecting the correction onto the look-ahead
//! vector (`:698-729`) — so a server correction is spread over the latency
//! window rather than teleported.
//!
//! The acceptance is the convergence: feeding a fixed linear server-sample
//! baseline, the output position lies on the baseline (at
//! `time - latency`) with the baseline velocity and yaw, a server-side
//! correction is approached smoothly over frames instead of popping, and
//! each `output` emits exactly one callback (the position/velocity/
//! direction update) in order.

use crate::movement_filter::{Dir3, Vec3};
use crate::player::EntityId;

/// `NUM_STORED_INPUTS` (`avatar_filter_helper.cpp:16`).
const NUM_STORED_INPUTS: usize = 8;
/// `NONZERO_TIME_DIFFERENCE` (`resetStoredInputs`, `:449`): the spacing of
/// the seeded samples and waypoints, chosen to avoid zero time differences.
const NONZERO_TIME_DIFFERENCE: f64 = 0.01;
/// `AvatarFilterSettings::s_latencyVelocity_` (`:30`).
const S_LATENCY_VELOCITY: f32 = 1.0;
/// `AvatarFilterSettings::s_latencyMinimum_` (`:39`).
const S_LATENCY_MINIMUM: f32 = 0.10;
/// `AvatarFilterSettings::s_latencyFrames_` (`:47`).
const S_LATENCY_FRAMES: f32 = 2.0;
/// `AvatarFilterSettings::s_latencyCurvePower_` (`:57`).
const S_LATENCY_CURVE_POWER: f32 = 2.0;
/// `AvatarFilterSettings::s_isActive_` (`:64`).
const S_IS_ACTIVE: bool = true;
/// `MATH_PI`.
const MATH_PI: f32 = std::f32::consts::PI;

/// A stored server sample (`StoredInput`, `avatar_filter_helper.hpp:35-45`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StoredInput {
    pub time: f64,
    pub space_id: u32,
    pub vehicle_id: Option<EntityId>,
    pub position: Vec3,
    pub position_error: Vec3,
    pub direction: Dir3,
    pub on_ground: bool,
}

impl Default for StoredInput {
    /// The default-constructed sample the unset waypoint boxes read as
    /// (all zeros).
    fn default() -> Self {
        Self {
            time: 0.0,
            space_id: 0,
            vehicle_id: None,
            position: (0.0, 0.0, 0.0),
            position_error: (0.0, 0.0, 0.0),
            direction: (0.0, 0.0, 0.0),
            on_ground: false,
        }
    }
}

/// A location in time and space for the output to move to or from
/// (`Waypoint`, `avatar_filter_helper.hpp:76-89`). `stored_input` carries
/// the sample the waypoint was built from (its error box drives the
/// overlapping-boxes check).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Waypoint {
    pub time: f64,
    pub space_id: u32,
    pub vehicle_id: Option<EntityId>,
    pub position: Vec3,
    pub direction: Dir3,
    pub stored_input: StoredInput,
}

impl Default for Waypoint {
    fn default() -> Self {
        Self {
            time: 0.0,
            space_id: 0,
            vehicle_id: None,
            position: (0.0, 0.0, 0.0),
            direction: (0.0, 0.0, 0.0),
            stored_input: StoredInput::default(),
        }
    }
}

/// The filter output: what the reference writes through its out-parameters
/// and the wrapper delivers to the entity as the position/velocity update
/// callback (`onEntityPositionUpdated`, `client/avatar_filter.cpp`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AvatarOutput {
    /// The returned `outputTime` (`:413`).
    pub output_time: f64,
    pub space_id: u32,
    pub vehicle_id: Option<EntityId>,
    pub position: Vec3,
    pub velocity: Vec3,
    /// `(roll, pitch, yaw)`; the roll is always 0 (`:555`).
    pub direction: Dir3,
}

/// `AvatarFilterHelper` (`lib/connection/avatar_filter_helper.cpp`).
#[derive(Debug, Clone, PartialEq)]
pub struct AvatarFilterHelper {
    /// The sample ring, `getStoredInput(i)` = the sample `i` back from the
    /// newest (`:736-748`).
    stored_inputs: [StoredInput; NUM_STORED_INPUTS],
    /// `currentInputIndex_`: the ring slot of the newest sample.
    current_input_index: usize,
    /// `inputCount_`.
    input_count: u32,
    /// `latency_`.
    latency: f32,
    /// `idealLatency_`.
    ideal_latency: f32,
    /// `timeOfLastOutput_`.
    time_of_last_output: f64,
    /// `gotNewInput_`.
    got_new_input: bool,
    /// `reset_`: the next input re-seeds the buffer (`:305-311`).
    reset_pending: bool,
    /// `previousWaypoint_`.
    previous_waypoint: Waypoint,
    /// `nextWaypoint_`.
    next_waypoint: Waypoint,
    /// `isActive()` (`s_isActive_`, `:64`).
    active: bool,
}

impl Default for AvatarFilterHelper {
    fn default() -> Self {
        Self::new()
    }
}

impl AvatarFilterHelper {
    /// The reference constructor state (`:180-194`): `latency_` 0, `reset_`
    /// true, so the first input seeds the buffer.
    pub fn new() -> Self {
        Self {
            stored_inputs: [StoredInput::default(); NUM_STORED_INPUTS],
            current_input_index: 0,
            input_count: 0,
            latency: 0.0,
            ideal_latency: 0.0,
            time_of_last_output: 0.0,
            got_new_input: false,
            reset_pending: true,
            previous_waypoint: Waypoint::default(),
            next_waypoint: Waypoint::default(),
            active: S_IS_ACTIVE,
        }
    }

    /// `isActive()` (`s_isActive_`).
    pub fn is_active(&self) -> bool {
        self.active
    }
    /// Override `s_isActive_` (the reference reads it from the settings
    /// watcher).
    pub fn set_active(&mut self, active: bool) {
        self.active = active;
    }

    /// `AvatarFilterHelper::reset` (`:274-277`): invalidate the inputs; the
    /// next one re-seeds the buffer.
    pub fn reset(&mut self, _time: f64) {
        self.reset_pending = true;
    }

    /// `AvatarFilterHelper::getLastInput` (`:782-798`): the newest stored
    /// input, or `None` while a reset is pending (none received since).
    pub fn get_last_input(&self) -> Option<StoredInput> {
        if self.reset_pending {
            None
        } else {
            Some(self.get_stored_input(0))
        }
    }

    /// `AvatarFilterHelper::latency` (`avatar_filter.hpp`).
    pub fn latency(&self) -> f32 {
        self.latency
    }

    /// `getStoredInput(index)` (`:736-748`): zero is the newest.
    fn get_stored_input(&self, index: usize) -> StoredInput {
        self.stored_inputs[(self.current_input_index + index) % NUM_STORED_INPUTS]
    }

    /// `resetStoredInputs` (`:444-486`): seed the ring with the reset sample
    /// (older copies spaced `NONZERO_TIME_DIFFERENCE` apart so no time
    /// difference is zero) and put both waypoints at the reset position.
    fn reset_stored_inputs(
        &mut self,
        time: f64,
        space_id: u32,
        vehicle_id: Option<EntityId>,
        position: Vec3,
        position_error: Vec3,
        direction: Dir3,
    ) {
        self.current_input_index = 0;
        self.input_count = 0;
        self.got_new_input = true;
        self.time_of_last_output = time;

        // `resolveOnGroundPosition` (`:459`) is engine-side (terrain); the
        // seeded sample is emitted as-is with on-ground false.
        let on_ground = false;
        for (i, stored_input) in self.stored_inputs.iter_mut().enumerate() {
            *stored_input = StoredInput {
                time: time - (i as f64) * NONZERO_TIME_DIFFERENCE,
                space_id,
                vehicle_id,
                position,
                position_error,
                direction,
                on_ground,
            };
        }

        self.latency = S_LATENCY_FRAMES * NONZERO_TIME_DIFFERENCE as f32;

        let next = Waypoint {
            time: time - NONZERO_TIME_DIFFERENCE,
            space_id,
            vehicle_id,
            position,
            direction,
            stored_input: StoredInput::default(),
        };
        let mut previous = next;
        previous.time -= NONZERO_TIME_DIFFERENCE;
        self.next_waypoint = next;
        self.previous_waypoint = previous;
    }

    /// `AvatarFilterHelper::input` (`:299-335`).
    pub fn input(
        &mut self,
        time: f64,
        space_id: u32,
        vehicle_id: Option<EntityId>,
        position: Vec3,
        position_error: Vec3,
        direction: Dir3,
    ) {
        if self.reset_pending {
            self.reset_stored_inputs(
                time,
                space_id,
                vehicle_id,
                position,
                position_error,
                direction,
            );
            self.reset_pending = false;
            return;
        }

        // `:313-316`: a stale sample is discarded.
        if time <= self.get_stored_input(0).time {
            return;
        }

        // `:318-319`: the ring slot moves back one, then holds the new
        // newest sample.
        self.current_input_index =
            (self.current_input_index + NUM_STORED_INPUTS - 1) % NUM_STORED_INPUTS;

        // `resolveOnGroundPosition` (`:330-331`) is engine-side (terrain);
        // the sample is stored as-is with on-ground false.
        let stored_input = StoredInput {
            time,
            space_id,
            vehicle_id,
            position,
            position_error,
            direction,
            on_ground: false,
        };
        self.stored_inputs[self.current_input_index] = stored_input;

        self.got_new_input = true;
        self.input_count += 1;
    }

    /// `AvatarFilterHelper::output` (`:355-414`): adapt the latency, then
    /// extract the sample at `time - latency`. The result is what the
    /// wrapper delivers to the entity as one position/velocity callback.
    pub fn output(&mut self, time: f64) -> AvatarOutput {
        // `:360-382`: adjust the ideal latency when something new arrived.
        if self.got_new_input {
            self.got_new_input = false;

            let max_latency_frame = self.input_count.min(NUM_STORED_INPUTS as u32) as i64 - 1;

            if max_latency_frame > 0 {
                let newest_time = self.get_stored_input(0).time;
                let older_time = self.get_stored_input(max_latency_frame as usize).time;

                let latency_frames = math_clamp(0.0, S_LATENCY_FRAMES, max_latency_frame as f32);

                let ratio =
                    (max_latency_frame as f64 - latency_frames as f64) / max_latency_frame as f64;

                self.ideal_latency = (time - math_lerp(ratio, older_time, newest_time)) as f32;
            }

            // `:381`: the ideal latency cannot be less than the minimum.
            self.ideal_latency = self.ideal_latency.max(S_LATENCY_MINIMUM);
        }

        // `:385-400`: move the latency towards the ideal.
        let d_time = (time - self.time_of_last_output) as f32;
        let d_latency = (S_LATENCY_VELOCITY * d_time)
            * ((self.ideal_latency - self.latency)
                .abs()
                .powf(S_LATENCY_CURVE_POWER))
            .min(1.0);

        if self.ideal_latency > self.latency {
            self.latency += d_latency;
            self.latency = self.latency.min(self.ideal_latency);
        } else {
            self.latency -= d_latency;
            self.latency = self.latency.max(self.ideal_latency);
        }

        // `:405`: recorded so the latency moves at a velocity independent of
        // how often output is called.
        self.time_of_last_output = time;

        // `:408-413`: find the position at `time - latency`.
        let output_time = time - self.latency as f64;
        let (space_id, vehicle_id, position, velocity, direction) = self.extract(output_time);
        AvatarOutput {
            output_time,
            space_id,
            vehicle_id,
            position,
            velocity,
            direction,
        }
    }

    /// `AvatarFilterHelper::extract` (`:507-556`).
    fn extract(&mut self, time: f64) -> (u32, Option<EntityId>, Vec3, Vec3, Dir3) {
        if !self.active {
            // `:513-524`: not interpolating — the newest sample verbatim.
            let most_recent = self.get_stored_input(0);
            return (
                most_recent.space_id,
                most_recent.vehicle_id,
                most_recent.position,
                (0.0, 0.0, 0.0),
                most_recent.direction,
            );
        }

        if time > self.next_waypoint.time {
            self.choose_next_waypoint(time);
        }

        // `:531-533`: the proportion of the way between the waypoints.
        let span = self.next_waypoint.time - self.previous_waypoint.time;
        let alpha = if span.abs() > f64::EPSILON {
            ((time - self.previous_waypoint.time) / span) as f32
        } else {
            0.0
        };

        // `:537-541`: the position and the waypoint slope.
        let position = vec3_lerp(
            self.previous_waypoint.position,
            self.next_waypoint.position,
            alpha,
        );
        let velocity = vec3_scale(
            vec3_sub(self.next_waypoint.position, self.previous_waypoint.position),
            1.0 / span as f32,
        );

        // `:543-555`: yaw and pitch lerp along the shortest arc, roll is 0.
        let yaw = angle_lerp(
            self.previous_waypoint.direction.2,
            self.next_waypoint.direction.2,
            alpha,
        );
        let pitch = angle_lerp(
            self.previous_waypoint.direction.1,
            self.next_waypoint.direction.1,
            alpha,
        );

        (
            self.next_waypoint.space_id,
            self.next_waypoint.vehicle_id,
            position,
            velocity,
            (0.0, pitch, yaw),
        )
    }

    /// `AvatarFilterHelper::chooseNextWaypoint` (`:586-733`).
    fn choose_next_waypoint(&mut self, time: f64) {
        // `:594-600`: no input data ahead of the time — stand still for one
        // frame by re-using the current waypoint with the time advanced.
        if self.get_stored_input(0).time < time {
            let current = self.next_waypoint;
            self.previous_waypoint = current;
            self.next_waypoint.time = time;
            return;
        }

        // `:602-610`: the oldest stored input still ahead of the time.
        let mut next_input_index = NUM_STORED_INPUTS - 1;
        while next_input_index > 0 {
            if self.get_stored_input(next_input_index).time > time {
                break;
            }
            next_input_index -= 1;
        }

        let look_ahead_input = self.get_stored_input(0);
        let next_input = self.get_stored_input(next_input_index);

        let mut new_waypoint = Waypoint {
            time: next_input.time,
            space_id: next_input.space_id,
            vehicle_id: next_input.vehicle_id,
            direction: next_input.direction,
            position: (0.0, 0.0, 0.0),
            stored_input: next_input,
        };

        // `:622-628`: both waypoints move into the new waypoint's coordinate
        // system. The reference transforms the positions; the port is
        // identity (see the divergence notes).
        let new_space = new_waypoint.space_id;
        let new_vehicle = new_waypoint.vehicle_id;
        self.previous_waypoint.space_id = new_space;
        self.previous_waypoint.vehicle_id = new_vehicle;
        self.next_waypoint.space_id = new_space;
        self.next_waypoint.vehicle_id = new_vehicle;

        // `:630-637`: extrapolate the look-ahead position from the current
        // waypoint pair.
        let previous = self.previous_waypoint;
        let current = self.next_waypoint;
        let look_ahead_relative_difference_in_time =
            ((look_ahead_input.time - previous.time) / (current.time - previous.time)) as f32;

        let mut look_ahead_position = vec3_lerp(
            previous.position,
            current.position,
            look_ahead_relative_difference_in_time,
        );

        // `:649-651`: clamp to the newest input's error box (the transforms
        // around it are identity).
        look_ahead_position = vec3_clamp(
            look_ahead_position,
            vec3_sub(look_ahead_input.position, look_ahead_input.position_error),
            vec3_add(look_ahead_input.position, look_ahead_input.position_error),
        );

        // `:661-686`: an unchanged coordinate system with a changed error box
        // that still overlaps the current one holds the position still.
        let new_bb = (
            vec3_sub(
                new_waypoint.stored_input.position,
                new_waypoint.stored_input.position_error,
            ),
            vec3_add(
                new_waypoint.stored_input.position,
                new_waypoint.stored_input.position_error,
            ),
        );
        let current_bb = (
            vec3_sub(
                current.stored_input.position,
                current.stored_input.position_error,
            ),
            vec3_add(
                current.stored_input.position,
                current.stored_input.position_error,
            ),
        );

        if new_waypoint.space_id == current.stored_input.space_id
            && new_waypoint.vehicle_id == current.stored_input.vehicle_id
            && !almost_equal(
                new_waypoint.stored_input.position_error,
                current.stored_input.position_error,
            )
            && aabb_intersects_box(new_bb.0, new_bb.1, current_bb.0, current_bb.1)
        {
            new_waypoint.position = current.position;
        } else {
            // `:689-696`: interpolate towards the clamped look-ahead.
            let proportionate_difference_in_time =
                ((next_input.time - current.time) / (look_ahead_input.time - current.time)) as f32;
            new_waypoint.position = vec3_lerp(
                current.position,
                look_ahead_position,
                proportionate_difference_in_time,
            );
        }

        // `:698-729`: constrain the waypoint to its input's error box by
        // projecting the correction onto the look-ahead vector.
        let next_bb_min = vec3_sub(next_input.position, next_input.position_error);
        let next_bb_max = vec3_add(next_input.position, next_input.position_error);
        if !aabb_intersects_point(next_bb_min, next_bb_max, new_waypoint.position) {
            let clamped_position = vec3_clamp(new_waypoint.position, next_bb_min, next_bb_max);

            let look_ahead_vector = vec3_sub(new_waypoint.position, current.position);
            let clamped_vector = vec3_sub(clamped_position, current.position);

            if vec3_len_sq(look_ahead_vector) > 0.0 {
                new_waypoint.position = vec3_add(
                    current.position,
                    vec3_project_onto(clamped_vector, look_ahead_vector),
                );
            } else {
                new_waypoint.position = current.position;
            }

            new_waypoint.position = vec3_clamp(new_waypoint.position, next_bb_min, next_bb_max);
        }

        // `:731-732`: the current waypoint becomes the previous one.
        self.previous_waypoint = current;
        self.next_waypoint = new_waypoint;
    }
}

/// `Math::lerp(t, a, b)` (`lib/math/mathdef.hpp:322-332`).
fn math_lerp(t: f64, a: f64, b: f64) -> f64 {
    a + t * (b - a)
}

/// `Math::clamp(minValue, value, maxValue)` (`mathdef.hpp`).
fn math_clamp(min: f32, value: f32, max: f32) -> f32 {
    if value < min {
        min
    } else if value > max {
        max
    } else {
        value
    }
}

fn vec3_sub(a: Vec3, b: Vec3) -> Vec3 {
    (a.0 - b.0, a.1 - b.1, a.2 - b.2)
}

fn vec3_add(a: Vec3, b: Vec3) -> Vec3 {
    (a.0 + b.0, a.1 + b.1, a.2 + b.2)
}

/// `Vector3::lerp(a, b, t) = a*(1-t) + b*t` (`vector3.ipp:147-151`).
fn vec3_lerp(a: Vec3, b: Vec3, t: f32) -> Vec3 {
    (
        a.0 * (1.0 - t) + b.0 * t,
        a.1 * (1.0 - t) + b.1 * t,
        a.2 * (1.0 - t) + b.2 * t,
    )
}

/// `Vector3::clamp(lower, upper)` (`vector3.ipp:160-167`).
fn vec3_clamp(v: Vec3, lower: Vec3, upper: Vec3) -> Vec3 {
    (
        math_clamp(lower.0, v.0, upper.0),
        math_clamp(lower.1, v.1, upper.1),
        math_clamp(lower.2, v.2, upper.2),
    )
}

fn vec3_dot(a: Vec3, b: Vec3) -> f32 {
    a.0 * b.0 + a.1 * b.1 + a.2 * b.2
}

fn vec3_len_sq(a: Vec3) -> f32 {
    vec3_dot(a, a)
}

/// `Vector3::projectOnto(v1, v2) = v2 * (v1·v2 / |v2|²)`
/// (`vector3.ipp:185-192`).
fn vec3_project_onto(v1: Vec3, v2: Vec3) -> Vec3 {
    let len_sq = vec3_len_sq(v2);
    if len_sq > 0.0 {
        vec3_scale(v2, vec3_dot(v1, v2) / len_sq)
    } else {
        (0.0, 0.0, 0.0)
    }
}

fn vec3_scale(a: Vec3, s: f32) -> Vec3 {
    (a.0 * s, a.1 * s, a.2 * s)
}

/// `Angle::sameSignAngle(angle, closest)` (`angle.ipp:198-212`): move
/// `closest` into the same ±π turn as `angle`.
fn same_sign_angle(angle: f32, closest: f32) -> f32 {
    if closest > angle + MATH_PI {
        closest - 2.0 * MATH_PI
    } else if closest < angle - MATH_PI {
        closest + 2.0 * MATH_PI
    } else {
        closest
    }
}

/// `Angle::lerp(a, b, alpha) = (sameSignAngle(a, b) - a) * alpha + a`
/// (`angle.ipp:264-267`).
fn angle_lerp(a: f32, b: f32, alpha: f32) -> f32 {
    (same_sign_angle(a, b) - a) * alpha + a
}

/// `intervalsIntersect` (`boundbox.ipp:279-283`) — inclusive on both ends.
fn aabb_intersects_box(min1: Vec3, max1: Vec3, min2: Vec3, max2: Vec3) -> bool {
    let ax = min2.0 <= max1.0 && min1.0 <= max2.0;
    let ay = min2.1 <= max1.1 && min1.1 <= max2.1;
    let az = min2.2 <= max1.2 && min1.2 <= max2.2;
    ax && ay && az
}

/// `AABB::intersects(const Vector3 &)` (`boundbox.ipp:303-307`): the
/// minimum faces are inclusive, the maximum exclusive.
fn aabb_intersects_point(min: Vec3, max: Vec3, v: Vec3) -> bool {
    v.0 >= min.0 && v.1 >= min.1 && v.2 >= min.2 && v.0 < max.0 && v.1 < max.1 && v.2 < max.2
}

/// The `almostEqual` of the error boxes (`:678`) — the port compares
/// exactly (see the divergence notes).
fn almost_equal(a: Vec3, b: Vec3) -> bool {
    a == b
}

/// A callback queued on the `AvatarFilter`, fired when the output time
/// passes its target time (`AvatarFilterCallback`,
/// `client/avatar_filter_callback.hpp`). The filter owns the callback and
/// drops it once it has been triggered.
pub trait AvatarFilterCallback {
    /// `targetTime()` (`avatar_filter_callback.hpp:26`).
    fn target_time(&self) -> f64;
    /// `onCallback(missedBy)` (`avatar_filter_callback.hpp:48-50`): how long
    /// past the target time the output fired. Return true to snap the output
    /// to the position at the target time for this tick only.
    fn on_callback(&mut self, missed_by: f64) -> bool;
}

/// The entity the `AvatarFilter` writes to (`MovementFilterTarget` plus the
/// entity-side callbacks in `AvatarFilter::output`). The reference target is
/// the `Entity` (`avatar_filter.cpp:151`).
pub trait AvatarFilterTarget {
    /// `setSpaceVehiclePositionAndDirection` (`avatar_filter.cpp:162-163`).
    fn set_space_vehicle_position_and_direction(
        &mut self,
        space_id: u32,
        vehicle_id: Option<EntityId>,
        position: Vec3,
        direction: Dir3,
    );
    /// `Entity::localVelocity(vel)` (`entity.hpp:147`).
    fn local_velocity(&mut self, velocity: Vec3);
    /// `entity.waitingForVehicle()` (`avatar_filter.cpp:166`): while a
    /// vehicle is pending the entity is not updated further.
    fn waiting_for_vehicle(&self) -> bool;
    /// `onEntityPositionUpdated` (`avatar_filter.cpp:173`): the filter's
    /// virtual, delivered to the entity delegate after a position update.
    fn on_entity_position_updated(&mut self);
}

/// `AvatarFilter` (`client/avatar_filter.cpp`): the server prediction
/// correction filter. It delegates the interpolation to the helper and, on
/// `output`, applies the position/velocity update to the entity, fires the
/// `onEntityPositionUpdated` callback, and triggers any queued callbacks
/// whose target time the output has passed (snapping the position to the
/// target time when a callback asks to).
pub struct AvatarFilter {
    /// `helper_` (`avatar_filter.cpp:59`).
    helper: AvatarFilterHelper,
    /// `callbackListHead_` — the callbacks, ordered by target time
    /// (`avatar_filter.cpp:60`, `:178-214`).
    callbacks: Vec<Box<dyn AvatarFilterCallback>>,
}

impl Default for AvatarFilter {
    fn default() -> Self {
        Self::new()
    }
}

impl AvatarFilter {
    /// `AvatarFilter::AvatarFilter` (`avatar_filter.cpp:57-65`).
    pub fn new() -> Self {
        Self {
            helper: AvatarFilterHelper::new(),
            callbacks: Vec::new(),
        }
    }

    /// `AvatarFilter::reset` (`:95-98`): invalidate the inputs.
    pub fn reset(&mut self, time: f64) {
        self.helper.reset(time);
    }

    /// `AvatarFilter::input` (`:120-130`).
    pub fn input(
        &mut self,
        time: f64,
        space_id: u32,
        vehicle_id: Option<EntityId>,
        position: Vec3,
        position_error: Vec3,
        direction: Dir3,
    ) {
        self.helper.input(
            time,
            space_id,
            vehicle_id,
            position,
            position_error,
            direction,
        );
    }

    /// `AvatarFilter::latency` (`avatar_filter.hpp`).
    pub fn latency(&self) -> f32 {
        self.helper.latency()
    }

    /// `AvatarFilter::getLastInput` (`:234-244`).
    pub fn get_last_input(&self) -> Option<StoredInput> {
        self.helper.get_last_input()
    }

    /// `AvatarFilter::addCallback` (`:252-256`): insert in time order (the
    /// callbacks fire in order of ascending target time).
    pub fn add_callback(&mut self, callback: Box<dyn AvatarFilterCallback>) {
        let new_target = callback.target_time();
        // `insertIntoList` (`avatar_filter_callback.cpp:39-60`): walk back
        // from the newest and insert after the last callback strictly
        // earlier in time, so ties insert before the existing group.
        let insert_at = self
            .callbacks
            .iter()
            .rposition(|c| c.target_time() < new_target)
            .map(|i| i + 1)
            .unwrap_or(0);
        self.callbacks.insert(insert_at, callback);
    }

    /// `AvatarFilter::output` (`:145-215`): update the entity to the
    /// estimated values at `time`, then fire the queued callbacks. Returns
    /// the output time the helper settled on.
    pub fn output(&mut self, time: f64, target: &mut dyn AvatarFilterTarget) -> f64 {
        let result = self.helper.output(time);

        target.set_space_vehicle_position_and_direction(
            result.space_id,
            result.vehicle_id,
            result.position,
            result.direction,
        );
        target.local_velocity(result.velocity);

        if target.waiting_for_vehicle() {
            // `:166-170`: a pending vehicle means the entity is not yet
            // updated further.
            return result.output_time;
        }

        // `:173`: the onEntityPositionUpdated callback.
        target.on_entity_position_updated();

        // `:178-214`: trigger the callbacks whose target time has passed.
        loop {
            let head_target = self.callbacks.first().map(|c| c.target_time());
            match head_target {
                Some(target_time) if target_time <= result.output_time => {
                    let mut callback = self.callbacks.remove(0);
                    if callback.on_callback(result.output_time - target_time) {
                        // `:199-208`: snap the output to the position at the
                        // target time for this tick.
                        let (space, vehicle, position, velocity, direction) =
                            self.helper.extract(target_time);
                        target.set_space_vehicle_position_and_direction(
                            space, vehicle, position, direction,
                        );
                        target.local_velocity(velocity);
                        target.on_entity_position_updated();
                    }
                    // The callback is dropped here (the reference
                    // `bw_safe_delete`).
                }
                _ => break,
            }
        }

        result.output_time
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SPACE: u32 = 7;
    const ERROR: Vec3 = (0.1, 0.1, 0.1);
    const YAW: f32 = 0.5;

    /// One server sample of a linear baseline: `position.x = t`.
    fn linear_sample(time: f64) -> (f64, u32, Option<EntityId>, Vec3, Vec3, Dir3) {
        (
            time,
            SPACE,
            None,
            (time as f32, 0.0, 0.0),
            ERROR,
            (0.0, 0.0, YAW),
        )
    }

    /// The acceptance: with a fixed linear server-sample baseline, the
    /// output converges onto the baseline — the emitted position lies on
    /// the line at the returned output time, with the baseline velocity and
    /// yaw, one callback per output.
    #[test]
    fn output_converges_to_the_server_baseline() {
        let mut f = AvatarFilterHelper::new();
        let mut callbacks = Vec::new();

        // Interleaved like the reference: a sample each frame, then an
        // output for that frame.
        for i in 0..400 {
            let t = i as f64 * 0.01;
            let (time, space, vehicle, position, error, direction) = linear_sample(t);
            f.input(time, space, vehicle, position, error, direction);
            callbacks.push(f.output(t + 0.005));
        }

        // The tail of the callback stream is converged: the position lies on
        // the baseline at the returned output time (whatever latency the
        // filter settled on), and the velocity and yaw match the baseline.
        // Note: `chooseNextWaypoint` clamps the extrapolated look-ahead to
        // the newest input's error box (`:649-651`), so a linear baseline
        // settles onto `output_time - error.x` (the box lower edge), not the
        // exact `output_time`.
        for out in callbacks.iter().rev().take(20) {
            assert!(
                (out.position.0 - (out.output_time as f32 - ERROR.0)).abs() < 1e-2,
                "position.x {} should equal the output time {} minus the error {:.2}",
                out.position.0,
                out.output_time,
                ERROR.0
            );
            assert_eq!(out.position.1, 0.0);
            assert_eq!(out.position.2, 0.0);
            assert!(
                (out.velocity.0 - 1.0).abs() < 1e-2,
                "velocity follows the baseline"
            );
            assert_eq!(out.velocity.1, 0.0);
            assert_eq!(out.velocity.2, 0.0);
            assert!(
                (out.direction.2 - YAW).abs() < 1e-2,
                "yaw follows the baseline"
            );
            assert_eq!(out.direction.1, 0.0, "pitch");
            assert_eq!(out.direction.0, 0.0, "roll is always 0");
            assert_eq!(out.space_id, SPACE);
            assert_eq!(out.vehicle_id, None);
        }
        assert_eq!(callbacks.len(), 400, "one callback per output");
    }

    /// The latency adapts toward the ideal: it starts at the seeded
    /// `s_latencyFrames_ * NONZERO_TIME_DIFFERENCE` and grows towards
    /// `s_latencyMinimum_`, never below it.
    #[test]
    fn latency_adapts_towards_the_ideal_minimum() {
        let mut f = AvatarFilterHelper::new();
        f.input(0.0, SPACE, None, (0.0, 0.0, 0.0), ERROR, (0.0, 0.0, YAW));
        let first = f.output(0.005);
        // The seed is s_latencyFrames_ * NONZERO_TIME_DIFFERENCE (0.02); the
        // first output has already nudged the latency towards the ideal
        // before extracting (`:385-400` runs before `:408`), so the latency
        // sits just above the seed.
        assert!(
            (f.latency() - 0.02).abs() < 1e-3,
            "the latency starts at the seed"
        );
        assert!((first.output_time - (0.005 - f.latency() as f64)).abs() < 1e-6);

        // Feed a long stream; the latency only ever grows towards the
        // minimum, so the output time lag increases monotonically.
        let mut last_lag = first.output_time - 0.005;
        for i in 1..300 {
            let t = i as f64 * 0.01;
            let (time, space, vehicle, position, error, direction) = linear_sample(t);
            f.input(time, space, vehicle, position, error, direction);
            let out = f.output(t + 0.005);
            let lag = out.output_time - (t + 0.005);
            assert!(
                lag <= last_lag + 1e-9,
                "the lag grows towards the minimum: {} after {}",
                lag,
                last_lag
            );
            last_lag = lag;
        }
        assert!(
            last_lag < -0.02,
            "the latency has moved past the seed towards the minimum"
        );
    }

    /// A stale sample (`time <= newest`) is discarded (`:313-316`).
    #[test]
    fn stale_input_is_discarded() {
        let mut f = AvatarFilterHelper::new();
        f.input(1.0, SPACE, None, (1.0, 0.0, 0.0), ERROR, (0.0, 0.0, YAW));
        f.input(0.5, SPACE, None, (99.0, 0.0, 0.0), ERROR, (0.0, 0.0, YAW));
        // The newest sample is still (1.0, 0, 0): an inactive filter emits
        // it verbatim.
        f.set_active(false);
        let out = f.output(2.0);
        assert_eq!(out.position, (1.0, 0.0, 0.0));
        assert_eq!(out.velocity, (0.0, 0.0, 0.0));
    }

    /// An inactive filter emits the newest sample verbatim with zero
    /// velocity (`:513-524`).
    #[test]
    fn inactive_filter_emits_the_newest_sample_verbatim() {
        let mut f = AvatarFilterHelper::new();
        f.set_active(false);
        f.input(1.0, SPACE, None, (3.0, 4.0, 5.0), ERROR, (0.1, 0.2, 0.3));
        let out = f.output(2.0);
        assert_eq!(out.position, (3.0, 4.0, 5.0));
        assert_eq!(out.velocity, (0.0, 0.0, 0.0));
        assert_eq!(out.direction, (0.1, 0.2, 0.3));
        assert_eq!(out.space_id, SPACE);
    }

    /// No input data ahead of the requested time — stand still for one
    /// frame (`:594-600`): the output holds the last waypoint position with
    /// zero velocity instead of extrapolating past the newest sample.
    #[test]
    fn no_input_ahead_stands_still() {
        let mut f = AvatarFilterHelper::new();
        // A short burst of samples then a long quiet stretch: outputs past
        // the newest sample hold the last position.
        for i in 0..20 {
            let t = i as f64 * 0.01;
            let (time, space, vehicle, position, error, direction) = linear_sample(t);
            f.input(time, space, vehicle, position, error, direction);
            f.output(t + 0.005);
        }
        // Output well past the newest sample (0.19): stands still.
        let out = f.output(5.0);
        assert!(
            (out.position.0 - 0.19).abs() < 0.15,
            "the output holds near the last sample, got {}",
            out.position.0
        );
        assert!(
            out.velocity.0.abs() < 0.2,
            "standing still has near-zero velocity, got {}",
            out.velocity.0
        );
    }

    /// `resetStoredInputs` re-seeds the buffer (`:444-486`): after a reset
    /// the filter restarts at the new sample, at the seeded latency.
    #[test]
    fn reset_re_seeds_the_filter() {
        let mut f = AvatarFilterHelper::new();
        for i in 0..50 {
            let t = i as f64 * 0.01;
            let (time, space, vehicle, position, error, direction) = linear_sample(t);
            f.input(time, space, vehicle, position, error, direction);
            f.output(t + 0.005);
        }
        // `AvatarFilter::reset` sets the pending flag; the next input
        // re-seeds the history at the new sample (`:274-277`, `:291-297`).
        // The reference does not auto-reset on a space change — the caller
        // resets explicitly.
        f.reset(100.0);
        f.input(
            100.0,
            SPACE,
            None,
            (100.0, 0.0, 0.0),
            ERROR,
            (0.0, 0.0, YAW),
        );
        let out = f.output(100.005);
        // The seeded latency restarts (just above 0.02 after the first
        // output's nudge, `:385-400`), so the output time is just behind the
        // reset sample.
        assert!(
            (f.latency() - 0.02).abs() < 1e-3,
            "the seeded latency restarted, got {}",
            f.latency()
        );
        assert!((out.output_time - (100.005 - f.latency() as f64)).abs() < 1e-6);
        // The seeded waypoints sit at the reset position (100,0,0), so the
        // output is at the reset sample's position.
        assert!(
            (out.position.0 - 100.0).abs() < 0.05,
            "near the reset sample"
        );
    }

    /// The shortest-arc yaw lerp (`angle.ipp:198-212`, `:264-267`): a yaw
    /// correction that wraps through ±π takes the short way.
    #[test]
    fn yaw_lerps_along_the_shortest_arc() {
        // From 3 rad to -3 rad: the shortest arc is through π (3.14…), not
        // back through 0.
        let lerped = angle_lerp(3.0, -3.0, 0.5);
        assert!(
            (lerped - std::f32::consts::PI).abs() < 1e-3,
            "the halfway point of the short arc is ±π, got {}",
            lerped
        );
        // Small angles lerp linearly.
        assert!((angle_lerp(0.0, 0.4, 0.5) - 0.2).abs() < 1e-6);
        // The endpoints: `sameSignAngle` wraps the target onto the shortest
        // arc (`angle.ipp:198-212`), so at alpha=1 the result is `b` or its
        // 2π-equivalent (`angle.ipp:264-267`).
        assert!((angle_lerp(3.0, -3.0, 0.0) - 3.0).abs() < 1e-6);
        let end = angle_lerp(3.0, -3.0, 1.0);
        let diff = (end - (-3.0)).abs();
        assert!(
            diff < 1e-6 || (diff - std::f32::consts::TAU).abs() < 1e-6,
            "the end is b or its wrapped equivalent, got {}",
            end
        );
    }

    /// The vector helpers match the reference formulas.
    #[test]
    fn vector_helpers_match_the_reference() {
        assert_eq!(
            vec3_lerp((0.0, 0.0, 0.0), (2.0, 4.0, 6.0), 0.25),
            (0.5, 1.0, 1.5)
        );
        assert_eq!(
            vec3_clamp((5.0, -5.0, 0.5), (0.0, 0.0, 0.0), (1.0, 1.0, 1.0)),
            (1.0, 0.0, 0.5)
        );
        // projectOnto(v1, v2) = v2 * (v1·v2 / |v2|²).
        let p = vec3_project_onto((2.0, 0.0, 0.0), (1.0, 1.0, 0.0));
        assert!((p.0 - 1.0).abs() < 1e-6 && (p.1 - 1.0).abs() < 1e-6);
        assert_eq!(vec3_len_sq((1.0, 2.0, 2.0)), 9.0);
    }

    /// The bounding-box predicates match the reference: box-vs-box is
    /// inclusive on both ends (`boundbox.ipp:285-300`), box-vs-point is
    /// `>= min && < max` (`:303-307`).
    #[test]
    fn box_predicates_match_the_reference() {
        // Touching boxes intersect (inclusive).
        assert!(aabb_intersects_box(
            (0.0, 0.0, 0.0),
            (1.0, 1.0, 1.0),
            (1.0, 1.0, 1.0),
            (2.0, 2.0, 2.0)
        ));
        // Separated boxes do not.
        assert!(!aabb_intersects_box(
            (0.0, 0.0, 0.0),
            (1.0, 1.0, 1.0),
            (1.1, 0.0, 0.0),
            (2.0, 1.0, 1.0)
        ));
        // The centre is inside; the max face is not (`<`).
        assert!(aabb_intersects_point(
            (0.0, 0.0, 0.0),
            (1.0, 1.0, 1.0),
            (0.5, 0.5, 0.5)
        ));
        assert!(!aabb_intersects_point(
            (0.0, 0.0, 0.0),
            (1.0, 1.0, 1.0),
            (1.0, 0.5, 0.5)
        ));
        assert!(aabb_intersects_point(
            (0.0, 0.0, 0.0),
            (1.0, 1.0, 1.0),
            (0.0, 0.5, 0.5)
        ));
    }

    /// A server correction within the error box is smoothed, not popped:
    /// when the baseline shifts sideways mid-stream the output approaches
    /// the corrected baseline over several frames.
    #[test]
    fn a_server_correction_is_smoothed_not_popped() {
        let mut f = AvatarFilterHelper::new();
        // Line A for the first half.
        for i in 0..150 {
            let t = i as f64 * 0.01;
            let (time, space, vehicle, position, error, direction) = linear_sample(t);
            f.input(time, space, vehicle, position, error, direction);
            f.output(t + 0.005);
        }
        // Line B: the corrected baseline offset sideways in y by 2.0. The
        // error box stays small: `chooseNextWaypoint` clamps the look-ahead
        // to the input's error box (`:649-651`), so a wide y error would pin
        // the lower edge at `2.0 - error.y` and never lift the output.
        let mut corrections = Vec::new();
        for i in 150..250 {
            let t = i as f64 * 0.01;
            let sample = (t, SPACE, None, (t as f32, 2.0, 0.0), ERROR, (0.0, 0.0, YAW));
            f.input(sample.0, sample.1, sample.2, sample.3, sample.4, sample.5);
            corrections.push(f.output(t + 0.005));
        }
        // No single callback jumps the whole 2.0 correction: the largest
        // per-frame step is a fraction of the correction (the reference
        // blends `proportionateDifferenceInTime` into each new waypoint,
        // `:689-696`), so the move is spread over several frames.
        let steps: Vec<f32> = corrections
            .windows(2)
            .map(|w| (w[1].position.1 - w[0].position.1).abs())
            .collect();
        let max_step = steps.iter().cloned().fold(0.0, f32::max);
        assert!(
            max_step < 1.5,
            "no frame jumps the whole correction, max step {}",
            max_step
        );
        assert!(steps.len() >= 5, "the correction spans several frames");
        // The tail is on the corrected baseline: y settles within the
        // corrected baseline's error box (`2.0 ± error.y`). The extrapolated
        // look-ahead overshoots during the transition, so the clamp
        // (`:649-651`) parks it on one of the box edges rather than exactly
        // on the baseline.
        let last = corrections.last().unwrap();
        assert!(
            (last.position.1 - 2.0).abs() <= ERROR.1 + 1e-3,
            "the output converges towards the corrected line, y={}",
            last.position.1
        );
    }

    /// The entity side of the filter: records the callbacks it receives, in
    /// order, so the callback baseline is observable.
    #[derive(Debug, Default)]
    struct RecordingEntity {
        events: Vec<EntityEvent>,
        waiting: bool,
    }

    /// One callback the entity received (`avatar_filter.cpp:162-173`).
    #[derive(Debug, Clone, Copy, PartialEq)]
    enum EntityEvent {
        SetPosition(Vec3),
        SetVelocity(Vec3),
        OnEntityPositionUpdated,
    }

    impl RecordingEntity {
        fn waiting(mut self, waiting: bool) -> Self {
            self.waiting = waiting;
            self
        }
    }

    impl AvatarFilterTarget for RecordingEntity {
        fn set_space_vehicle_position_and_direction(
            &mut self,
            _space_id: u32,
            _vehicle_id: Option<EntityId>,
            position: Vec3,
            _direction: Dir3,
        ) {
            self.events.push(EntityEvent::SetPosition(position));
        }

        fn local_velocity(&mut self, velocity: Vec3) {
            self.events.push(EntityEvent::SetVelocity(velocity));
        }

        fn waiting_for_vehicle(&self) -> bool {
            self.waiting
        }

        fn on_entity_position_updated(&mut self) {
            self.events.push(EntityEvent::OnEntityPositionUpdated);
        }
    }

    /// A queued callback: records when it fired (into a shared log) and
    /// whether it wants to snap.
    struct TestCallback {
        target_time: f64,
        snap: bool,
        fired: std::cell::Cell<Option<f64>>,
        log: std::rc::Rc<std::cell::RefCell<Vec<f64>>>,
    }

    impl TestCallback {
        fn new(
            target_time: f64,
            snap: bool,
            log: &std::rc::Rc<std::cell::RefCell<Vec<f64>>>,
        ) -> Self {
            Self {
                target_time,
                snap,
                fired: std::cell::Cell::new(None),
                log: log.clone(),
            }
        }
    }

    impl AvatarFilterCallback for TestCallback {
        fn target_time(&self) -> f64 {
            self.target_time
        }
        fn on_callback(&mut self, missed_by: f64) -> bool {
            self.fired.set(Some(missed_by));
            self.log.borrow_mut().push(self.target_time);
            self.snap
        }
    }

    /// Warm the filter up on the linear baseline, interleaved like the
    /// reference (a sample each frame, then an output for it). Returns the
    /// last output wall time.
    fn warm_filter(filter: &mut AvatarFilter, frames: usize) -> f64 {
        let mut last = 0.0;
        for i in 0..frames {
            let t = i as f64 * 0.01;
            let (time, space, vehicle, position, error, direction) = linear_sample(t);
            filter.input(time, space, vehicle, position, error, direction);
            last = t + 0.005;
            filter.output(last, &mut RecordingEntity::default());
        }
        last
    }

    /// The wrapper's base callback order: each output applies the position
    /// update, the velocity, then `onEntityPositionUpdated` (`:162-173`).
    #[test]
    fn output_applies_the_callbacks_in_the_reference_order() {
        let mut filter = AvatarFilter::new();
        warm_filter(&mut filter, 60);
        // One more frame in the same pattern.
        let t = 60.0 * 0.01;
        let (time, space, vehicle, position, error, direction) = linear_sample(t);
        filter.input(time, space, vehicle, position, error, direction);
        let mut entity = RecordingEntity::default();
        let out_time = filter.output(t + 0.005, &mut entity);

        assert_eq!(
            entity.events.len(),
            3,
            "one position, one velocity, one callback"
        );
        match entity.events[0] {
            EntityEvent::SetPosition(p) => {
                assert!(
                    (p.0 - (out_time as f32 - ERROR.0)).abs() < 1e-3,
                    "the position lies on the baseline at the output time (offset by the error box), got {:?}",
                    p
                );
                assert_eq!(p.1, 0.0);
                assert_eq!(p.2, 0.0);
            }
            _ => panic!("the first event is the position update"),
        }
        let vel = match entity.events[1] {
            EntityEvent::SetVelocity(v) => v,
            _ => panic!("the second event is the velocity update"),
        };
        assert!(
            (vel.0 - 1.0).abs() < 1e-3,
            "the velocity follows the baseline, got {:?}",
            vel
        );
        assert_eq!((vel.1, vel.2), (0.0, 0.0));
        assert_eq!(entity.events[2], EntityEvent::OnEntityPositionUpdated);
    }

    /// A waiting vehicle short-circuits the update after the base position
    /// and velocity (`:166-170`): no `onEntityPositionUpdated` and no
    /// callbacks.
    #[test]
    fn a_waiting_vehicle_skips_the_rest_of_the_update() {
        let mut filter = AvatarFilter::new();
        warm_filter(&mut filter, 60);
        let log = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        filter.add_callback(Box::new(TestCallback::new(0.5, false, &log)));

        let t = 60.0 * 0.01;
        let (time, space, vehicle, position, error, direction) = linear_sample(t);
        filter.input(time, space, vehicle, position, error, direction);
        let mut entity = RecordingEntity::default().waiting(true);
        filter.output(t + 0.005, &mut entity);

        // Exactly the base position and velocity, no callback, and the
        // queued callback did not fire.
        assert_eq!(entity.events.len(), 2);
        assert!(matches!(entity.events[0], EntityEvent::SetPosition(_)));
        let vel = match entity.events[1] {
            EntityEvent::SetVelocity(v) => v,
            _ => panic!("the second event is the velocity update"),
        };
        assert!(
            (vel.0 - 1.0).abs() < 1e-3,
            "the velocity follows the baseline, got {:?}",
            vel
        );
        assert!(
            log.borrow().is_empty(),
            "the waiting vehicle stops the callbacks"
        );
    }

    /// The callback queue fires in order of target time once the output time
    /// passes each target time (`:178-214`), and a callback whose time has
    /// not passed stays queued.
    #[test]
    fn callbacks_fire_in_target_time_order() {
        let mut filter = AvatarFilter::new();
        warm_filter(&mut filter, 80);
        let log = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        filter.add_callback(Box::new(TestCallback::new(0.70, false, &log)));
        filter.add_callback(Box::new(TestCallback::new(0.74, false, &log)));
        filter.add_callback(Box::new(TestCallback::new(0.80, false, &log)));

        // One more frame: the output time (~0.78) passes 0.70 and 0.74 but
        // not 0.80.
        let t = 80.0 * 0.01;
        let (time, space, vehicle, position, error, direction) = linear_sample(t);
        filter.input(time, space, vehicle, position, error, direction);
        let mut entity = RecordingEntity::default();
        let out_time = filter.output(t + 0.005, &mut entity);
        assert!(
            out_time > 0.70 && out_time < 0.80,
            "the output time straddles the targets"
        );

        assert_eq!(
            log.borrow().clone(),
            vec![0.70, 0.74],
            "the passed callbacks fire in target-time order"
        );
        // Non-snapping callbacks fire without re-extracting, so only the
        // base triplet is delivered (`:190-209`).
        assert_eq!(entity.events.len(), 3, "the base update only");
    }

    /// A callback that asks to snap rolls the output back to its own target
    /// time (`:199-208`): the position is re-extracted at the target time.
    #[test]
    fn a_snapping_callback_repositions_the_entity() {
        let mut filter = AvatarFilter::new();
        warm_filter(&mut filter, 80);
        let log = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        filter.add_callback(Box::new(TestCallback::new(0.70, true, &log)));

        let t = 80.0 * 0.01;
        let (time, space, vehicle, position, error, direction) = linear_sample(t);
        filter.input(time, space, vehicle, position, error, direction);
        let mut entity = RecordingEntity::default();
        filter.output(t + 0.005, &mut entity);

        // The base update, then the snapped re-extraction at the target
        // time — on the baseline offset by the error box (0.70 - error.x).
        let positions: Vec<Vec3> = entity
            .events
            .iter()
            .filter_map(|e| match e {
                EntityEvent::SetPosition(p) => Some(*p),
                _ => None,
            })
            .collect();
        assert_eq!(
            positions.len(),
            2,
            "the base and the snapped update, got {:?}",
            entity.events
        );
        assert!(
            (positions[1].0 - (0.70 - ERROR.0)).abs() < 1e-2,
            "the snap re-extracts at the target time, got {:?}",
            positions[1]
        );
        assert_eq!(positions[1].1, 0.0);
    }

    /// Fired callbacks are removed from the queue (`:211-213`): a second
    /// output does not re-fire them.
    #[test]
    fn fired_callbacks_are_removed() {
        let mut filter = AvatarFilter::new();
        warm_filter(&mut filter, 80);
        let log = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        filter.add_callback(Box::new(TestCallback::new(0.70, false, &log)));

        let mut entity = RecordingEntity::default();
        let (time, space, vehicle, position, error, direction) = linear_sample(0.80);
        filter.input(time, space, vehicle, position, error, direction);
        filter.output(0.805, &mut entity);
        assert_eq!(log.borrow().len(), 1, "fired once");
        filter.output(0.815, &mut entity);
        assert_eq!(log.borrow().len(), 1, "not re-fired");
    }

    /// `getLastInput` reports the newest sample, and `None` after a reset.
    #[test]
    fn get_last_input_tracks_the_newest_sample() {
        let mut f = AvatarFilterHelper::new();
        assert_eq!(f.get_last_input(), None, "nothing received yet");
        f.input(1.0, SPACE, None, (2.0, 3.0, 4.0), ERROR, (0.0, 0.0, YAW));
        let last = f.get_last_input().unwrap();
        assert_eq!(last.position, (2.0, 3.0, 4.0));
        f.reset(5.0);
        assert_eq!(f.get_last_input(), None, "a reset invalidates the inputs");
    }
}
