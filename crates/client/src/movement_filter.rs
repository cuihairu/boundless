//! Movement filtering (BW-11-02-04).
//!
//! A movement filter is attached to an entity to process its `volatile`
//! members (position, yaw, pitch, roll): the network layer feeds best-effort
//! input samples, and the filter produces a visually pleasing trajectory
//! (`lib/connection/movement_filter.hpp:14-28`). The common base is
//! `MovementFilter` (`movement_filter.hpp:17-108`): `reset`, `input`, `output`
//! and `getLastInput`, with the output going to a `MovementFilterTarget`.
//!
//! The canonical filter is `DumbFilter` (`client/dumb_filter.cpp`), the one
//! that just tracks the most recent trusted input. Its `input`
//! (`dumb_filter.cpp:109-167`) is a small state machine: a stale sample
//! (`time < time_`, `:119-122`) is discarded; when the space/vehicle changed
//! or the position error is unchanged the input is trusted (`:129-139`); when
//! the error changed but the old position still lies inside the new error box
//! the position is kept and the more accurate error is retained (`:140-152`);
//! otherwise the input is trusted even if it pops visually (`:153-164`). The
//! `output` (`:178-218`) emits the stored position and direction (resolving
//! the on-ground `y < -12000` sentinel through the terrain), and
//! `getLastInput` (`:237-257`) returns the last trusted sample.
//!
//! The acceptance is the trajectory: feeding a fixed sequence of input
//! samples, the emitted output samples follow the trusted inputs exactly, and
//! the error-box rules decide which sample is trusted.

use crate::player::EntityId;

/// A position (`Position3D`).
pub type Vec3 = (f32, f32, f32);
/// A direction, `(roll, pitch, yaw)` (`Direction3D`).
pub type Dir3 = (f32, f32, f32);

/// The sink a filter outputs to (`MovementFilterTarget`,
/// `lib/connection/movement_filter.hpp:10`).
pub trait FilterTarget {
    /// `MovementFilterTarget::setSpaceVehiclePositionAndDirection` — the
    /// entity's volatile members are set to this sample (`dumb_filter.cpp:214`).
    fn set_space_vehicle_position_and_direction(
        &mut self,
        space_id: u32,
        vehicle_id: Option<EntityId>,
        position: Vec3,
        direction: Dir3,
    );
}

/// The last input the filter received (`getLastInput`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LastInput {
    pub time: f64,
    pub space_id: u32,
    pub vehicle_id: Option<EntityId>,
    pub position: Vec3,
    pub position_error: Vec3,
    pub direction: Dir3,
}

/// The common base of all filters (`MovementFilter`,
/// `lib/connection/movement_filter.hpp:17-108`).
pub trait MovementFilter {
    /// `reset(time)` (`movement_filter.hpp:28`).
    fn reset(&mut self, time: f64);
    /// `input(...)` (`movement_filter.hpp:40-42`) — give the filter a sample.
    fn input(
        &mut self,
        time: f64,
        space_id: u32,
        vehicle_id: Option<EntityId>,
        position: Vec3,
        position_error: Vec3,
        direction: Dir3,
    );
    /// `output(time, target)` (`movement_filter.hpp:51`) — emit a position.
    fn output(&mut self, time: f64, target: &mut dyn FilterTarget);
    /// `getLastInput(...)` (`movement_filter.hpp:63-65`).
    fn get_last_input(&self) -> Option<LastInput>;
}

/// `DumbFilter` (`client/dumb_filter.cpp`): tracks the last trusted input and
/// emits it, with the error-box trust logic deciding which sample is trusted.
#[derive(Debug, Clone, PartialEq)]
pub struct DumbFilter {
    /// `time_`, initialised to `-1000.0` (`dumb_filter.cpp:67`).
    time: f64,
    space_id: u32,
    vehicle_id: Option<EntityId>,
    position: Vec3,
    position_error: Vec3,
    direction: Dir3,
}

impl DumbFilter {
    /// The reference constructor: `time_(-1000.0)` (`dumb_filter.cpp:65-75`).
    pub fn new() -> Self {
        Self {
            time: -1000.0,
            space_id: 0,
            vehicle_id: None,
            position: (0.0, 0.0, 0.0),
            position_error: (0.0, 0.0, 0.0),
            direction: (0.0, 0.0, 0.0),
        }
    }
}

impl Default for DumbFilter {
    fn default() -> Self {
        Self::new()
    }
}

impl MovementFilter for DumbFilter {
    /// `DumbFilter::reset` (`dumb_filter.cpp:84-88`).
    fn reset(&mut self, _time: f64) {
        self.time = -1000.0;
        self.position_error = (0.0, 0.0, 0.0);
    }

    /// `DumbFilter::input` (`dumb_filter.cpp:109-167`).
    fn input(
        &mut self,
        time: f64,
        space_id: u32,
        vehicle_id: Option<EntityId>,
        position: Vec3,
        position_error: Vec3,
        direction: Dir3,
    ) {
        // `:119-122`: a stale sample is discarded.
        if time < self.time {
            return;
        }
        self.time = time;

        // `:129-139`: same coordinate system and accuracy (or a changed
        // coordinate system) → trust the input.
        let error_unchanged = self.position_error == position_error;
        if self.space_id != space_id || self.vehicle_id != vehicle_id || error_unchanged {
            self.space_id = space_id;
            self.vehicle_id = vehicle_id;
            self.position = position;
            self.position_error = position_error;
        } else if error_box_contains(position, position_error, self.position) {
            // `:140-152`: the error changed but the old position still lies
            // inside the new error box → don't move, keep the more accurate
            // (smaller) error.
            if error_len_sq(position_error) < error_len_sq(self.position_error) {
                self.position_error = position_error;
            }
        } else {
            // `:153-164`: the old position is no longer inside the new error
            // box → trust the input (may pop visually).
            self.position = position;
            self.position_error = position_error;
        }

        self.direction = direction;
    }

    /// `DumbFilter::output` (`dumb_filter.cpp:178-218`).
    fn output(&mut self, _time: f64, target: &mut dyn FilterTarget) {
        // The on-ground `y < -12000` sentinel is resolved through the terrain
        // in the reference (`:188-212`); that is engine-side (see the module
        // divergence notes), so the stored position is emitted as-is.
        target.set_space_vehicle_position_and_direction(
            self.space_id,
            self.vehicle_id,
            self.position,
            self.direction,
        );
    }

    /// `DumbFilter::getLastInput` (`dumb_filter.cpp:237-257`).
    fn get_last_input(&self) -> Option<LastInput> {
        if self.time == -1000.0 {
            return None;
        }
        Some(LastInput {
            time: self.time,
            space_id: self.space_id,
            vehicle_id: self.vehicle_id,
            position: self.position,
            position_error: self.position_error,
            direction: self.direction,
        })
    }
}

/// Whether the axis-aligned error box `[pos-err, pos+err]` contains `point`
/// (`BoundingBox::intersects`, `dumb_filter.cpp:140`).
fn error_box_contains(position: Vec3, position_error: Vec3, point: Vec3) -> bool {
    (point.0 - position.0).abs() <= position_error.0
        && (point.1 - position.1).abs() <= position_error.1
        && (point.2 - position.2).abs() <= position_error.2
}

/// `Vector3::lengthSquared` (`dumb_filter.cpp:148`).
fn error_len_sq(v: Vec3) -> f32 {
    v.0 * v.0 + v.1 * v.1 + v.2 * v.2
}

/// A target that records what it is told, so the emitted trajectory is
/// observable.
#[derive(Debug, Default, PartialEq)]
pub struct RecordingTarget {
    pub calls: Vec<(u32, Option<EntityId>, Vec3, Dir3)>,
}

impl FilterTarget for RecordingTarget {
    fn set_space_vehicle_position_and_direction(
        &mut self,
        space_id: u32,
        vehicle_id: Option<EntityId>,
        position: Vec3,
        direction: Dir3,
    ) {
        self.calls.push((space_id, vehicle_id, position, direction));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The acceptance: a fixed input-sample trajectory is emitted unchanged —
    /// the output follows the trusted inputs sample by sample.
    #[test]
    fn input_samples_produce_the_same_trajectory() {
        let mut f = DumbFilter::new();
        let mut target = RecordingTarget::default();
        let samples = [
            // (time, position, direction)
            (0.0, (0.0, 0.0, 0.0), (0.0, 0.0, 0.0)),
            (0.5, (1.0, 0.0, 0.0), (0.0, 0.0, 0.5)),
            (1.0, (2.0, 1.0, 0.0), (0.0, 0.0, 1.0)),
        ];
        for (time, pos, dir) in samples {
            f.input(time, 7, None, pos, (0.1, 0.1, 0.1), dir);
            f.output(time, &mut target);
        }
        assert_eq!(
            target.calls,
            vec![
                (7, None, (0.0, 0.0, 0.0), (0.0, 0.0, 0.0)),
                (7, None, (1.0, 0.0, 0.0), (0.0, 0.0, 0.5)),
                (7, None, (2.0, 1.0, 0.0), (0.0, 0.0, 1.0)),
            ],
            "the output trajectory matches the input samples"
        );
    }

    /// A stale sample (`time < time_`) is discarded (`dumb_filter.cpp:119-122`).
    #[test]
    fn stale_input_is_discarded() {
        let mut f = DumbFilter::new();
        f.input(
            1.0,
            7,
            None,
            (10.0, 0.0, 0.0),
            (0.1, 0.1, 0.1),
            (0.0, 0.0, 0.0),
        );
        f.input(
            0.5,
            7,
            None,
            (99.0, 0.0, 0.0),
            (0.1, 0.1, 0.1),
            (0.0, 0.0, 0.0),
        );
        let last = f.get_last_input().unwrap();
        assert_eq!(last.position, (10.0, 0.0, 0.0), "the newer sample is kept");
    }

    /// A changed space or vehicle is always trusted (`dumb_filter.cpp:129-139`).
    #[test]
    fn changed_coordinate_system_is_trusted() {
        let mut f = DumbFilter::new();
        f.input(
            0.0,
            7,
            None,
            (1.0, 0.0, 0.0),
            (1.0, 1.0, 1.0),
            (0.0, 0.0, 0.0),
        );
        // Same space/vehicle, unchanged error → trusted.
        f.input(
            1.0,
            7,
            None,
            (5.0, 0.0, 0.0),
            (1.0, 1.0, 1.0),
            (0.0, 0.0, 0.0),
        );
        assert_eq!(f.get_last_input().unwrap().position, (5.0, 0.0, 0.0));
        // Changed vehicle → trusted even though the error differs.
        f.input(
            2.0,
            7,
            Some(EntityId(3)),
            (9.0, 0.0, 0.0),
            (2.0, 2.0, 2.0),
            (0.0, 0.0, 0.0),
        );
        assert_eq!(f.get_last_input().unwrap().position, (9.0, 0.0, 0.0));
    }

    /// The error changed but the old position still lies inside the new error
    /// box: the position is kept and the more accurate error retained
    /// (`dumb_filter.cpp:140-152`).
    #[test]
    fn error_box_containment_keeps_the_old_position() {
        let mut f = DumbFilter::new();
        f.input(
            0.0,
            7,
            None,
            (10.0, 0.0, 0.0),
            (4.0, 4.0, 4.0),
            (0.0, 0.0, 0.0),
        );
        // New sample at (10.5,0,0) with a tighter error box (0.5) that still
        // contains the old position (10,0,0): the box spans [10.0,11.0].
        f.input(
            1.0,
            7,
            None,
            (10.5, 0.0, 0.0),
            (0.5, 0.5, 0.5),
            (0.0, 0.0, 0.0),
        );
        let last = f.get_last_input().unwrap();
        assert_eq!(last.position, (10.0, 0.0, 0.0), "the old position is kept");
        assert_eq!(
            last.position_error,
            (0.5, 0.5, 0.5),
            "the more accurate error is retained"
        );
    }

    /// The error changed and the old position is outside the new error box:
    /// the input is trusted even if it pops (`dumb_filter.cpp:153-164`).
    #[test]
    fn error_box_violation_trusts_the_input() {
        let mut f = DumbFilter::new();
        f.input(
            0.0,
            7,
            None,
            (10.0, 0.0, 0.0),
            (4.0, 4.0, 4.0),
            (0.0, 0.0, 0.0),
        );
        // The new sample is far away and its tight error box excludes the old
        // position.
        f.input(
            1.0,
            7,
            None,
            (100.0, 0.0, 0.0),
            (0.5, 0.5, 0.5),
            (0.0, 0.0, 0.0),
        );
        assert_eq!(
            f.get_last_input().unwrap().position,
            (100.0, 0.0, 0.0),
            "a pop is accepted"
        );
    }

    /// `reset` invalidates the state (`dumb_filter.cpp:84-88`).
    #[test]
    fn reset_invalidates_the_last_input() {
        let mut f = DumbFilter::new();
        f.input(
            1.0,
            7,
            None,
            (1.0, 0.0, 0.0),
            (1.0, 1.0, 1.0),
            (0.0, 0.0, 0.0),
        );
        assert!(f.get_last_input().is_some());
        f.reset(5.0);
        assert_eq!(f.get_last_input(), None, "reset clears the state");
        assert_eq!(f.position_error, (0.0, 0.0, 0.0));
    }

    /// `getLastInput` reports `None` before any input (`dumb_filter.cpp:245-248`).
    #[test]
    fn get_last_input_is_none_before_any_input() {
        let f = DumbFilter::new();
        assert_eq!(f.get_last_input(), None);
    }

    /// `input` updates the direction too (`dumb_filter.cpp:166`).
    #[test]
    fn direction_is_emitted_with_the_position() {
        let mut f = DumbFilter::new();
        f.input(
            1.0,
            7,
            None,
            (1.0, 2.0, 3.0),
            (0.1, 0.1, 0.1),
            (0.2, 0.3, 0.4),
        );
        let mut target = RecordingTarget::default();
        f.output(1.0, &mut target);
        assert_eq!(target.calls[0].3, (0.2, 0.3, 0.4));
    }
}
