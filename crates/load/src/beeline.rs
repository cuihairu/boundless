//! The `Beeline` movement controller (`server/tools/bots/beeline_controller.hpp`).
//!
//! The straight-line controller: each tick it walks one `speed * dTime`
//! stride toward a fixed destination, turning the yaw to face it. The gate
//! is strict (`destVec.length() > distance`), so a bot never overshoots —
//! it parks up to one stride short of the destination and stays there.
//! `nextStep` always returns `true`; the reference caller ignores the
//! return anyway (client_app.cpp:1118).

use foundation::math::vector::Vector3;

use crate::bot::{MovementController, MovementFactory};

/// `Beeline::BeelineController` (beeline_controller.hpp:14-29).
pub struct BeelineController {
    /// `destinationPos_` — the fixed target the controller walks toward.
    destination_pos: Vector3,
}

impl BeelineController {
    /// `BeelineController( destinationPos )`.
    pub fn new(destination_pos: Vector3) -> Self {
        Self { destination_pos }
    }
}

impl MovementController for BeelineController {
    /// `nextStep` (beeline_controller.cpp:19-37): step `speed * dTime`
    /// along the normalized direction to the destination; the yaw tracks
    /// the step direction whenever the bot actually moves. The strict `>`
    /// means a bot within one stride of its destination stops moving
    /// (and stops turning) rather than stepping past it.
    fn next_step(&mut self, speed: &mut f32, dtime: f32, pos: &mut Vector3, yaw: &mut f32) -> bool {
        let distance = *speed * dtime;
        let dest_vec = self.destination_pos - *pos;
        let length = dest_vec.length();
        if length > distance {
            *pos = *pos + dest_vec * (distance / length);
            *yaw = dest_vec.yaw();
        }
        true
    }
}

/// `BeelineControllerFactory` (beeline_controller.cpp:49-80): the
/// `"Beeline"` entry for `moveType`. The move data is `"x,y,z"`; the
/// factory reads only the destination and leaves the bot's starting
/// position alone (beeline_controller.cpp:72-76).
pub struct BeelineControllerFactory;

impl MovementFactory for BeelineControllerFactory {
    fn create(
        &self,
        data: &str,
        _speed: f32,
        _start_pos: &mut Vector3,
    ) -> Option<Box<dyn MovementController>> {
        Some(Box::new(BeelineController::new(parse_destination(data))))
    }
}

/// The factory's destination parse (beeline_controller.cpp:62-70): the
/// x/y/z segments sit around the string's first and last comma.
///
/// The reference slices with `size_t` arithmetic, so the y segment's
/// count `com2 - com1 - 1` wraps to a huge value whenever the two comma
/// indices coincide (one comma, or none) and the segment then runs to the
/// end of the string; `npos + 1` wraps to 0 for the z segment the same
/// way. The arms below reproduce those degenerate shapes: `"1,2"` lands
/// on `(1, 2, 2)` and `"5"` on `(5, 5, 5)`.
///
/// `atof` also differs from a Rust `parse` on junk: `atof("3abc")` is
/// `3.0` while a strict parse fails. Well-formed `"x,y,z"` triples are
/// the contract — a segment that does not parse (including surrounding
/// whitespace only partly tolerated) lands on `0.0`, matching `atof`'s
/// zero-on-fail behaviour.
fn parse_destination(data: &str) -> Vector3 {
    let com1 = data.find(',');
    let com2 = data.rfind(',');
    // x = data.substr( 0, com1 )
    let x = atof(&data[..com1.unwrap_or(data.len())]);
    // y = data.substr( com1 + 1, com2 - com1 - 1 )
    let y = match com1 {
        Some(i) => match com2 {
            Some(j) if j > i => atof(&data[i + 1..j]),
            _ => atof(&data[i + 1..]),
        },
        None => atof(data),
    };
    // z = data.substr( com2 + 1 )
    let z = match com2 {
        Some(j) => atof(&data[j + 1..]),
        None => atof(data),
    };
    Vector3::new(x, y, z)
}

/// `atof` for a segment: a value, or `0.0` when it does not parse.
fn atof(text: &str) -> f32 {
    text.trim().parse().unwrap_or(0.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fleet::ControllerRegistry;
    use std::f32::consts::{FRAC_PI_2, FRAC_PI_4};

    /// The reference fixed route (beeline_controller.cpp:19-37): start at
    /// the origin, destination `(10, 0, 0)`, speed 2, dTime 1 — strides
    /// of 2 land on 2, 4, 6, 8, and the fifth tick, where the remaining
    /// distance equals the stride, does not move the bot (strict `>`).
    /// The bot parks one stride short and never overshoots.
    #[test]
    fn the_reference_route_is_reproduced_step_for_step() {
        let mut controller = BeelineController::new(Vector3::new(10.0, 0.0, 0.0));
        let mut pos = Vector3::new(0.0, 0.0, 0.0);
        let mut yaw = 0.0;
        let mut speed = 2.0;

        let route = [2.0, 4.0, 6.0, 8.0, 8.0];
        for (tick, &x) in route.iter().enumerate() {
            assert!(controller.next_step(&mut speed, 1.0, &mut pos, &mut yaw));
            assert_eq!(pos.x, x, "tick {}", tick + 1);
            assert_eq!(pos.y, 0.0);
            assert_eq!(pos.z, 0.0);
        }

        // Yaw tracked the direction on every moved tick and holds its
        // last value through the parked tick (atan2f( 2, 0 )).
        assert_eq!(yaw, FRAC_PI_2);
        // The controller never slows the bot down.
        assert_eq!(speed, 2.0);
    }

    /// On a diagonal destination the yaw is `atan2f( x, z )` — for
    /// `(10, 0, 10)` that is pi/4 — and the first stride is a normalized
    /// `speed * dTime` step along the diagonal.
    #[test]
    fn the_yaw_faces_the_destination_on_a_diagonal() {
        let mut controller = BeelineController::new(Vector3::new(10.0, 0.0, 10.0));
        let mut pos = Vector3::new(0.0, 0.0, 0.0);
        let mut yaw = 0.0;
        let mut speed = 2.0;

        assert!(controller.next_step(&mut speed, 1.0, &mut pos, &mut yaw));
        assert_eq!(yaw, FRAC_PI_4);
        // A stride of 2 along the (1, 0, 1) direction.
        assert!((pos.x - 1.4142136).abs() < 1e-5);
        assert_eq!(pos.y, 0.0);
        assert!((pos.z - 1.4142136).abs() < 1e-5);
    }

    /// An already-arrived bot neither moves nor turns, and `nextStep`
    /// still returns `true` (the return is always true; the reference
    /// caller ignores it regardless, client_app.cpp:1118).
    #[test]
    fn an_arrived_bot_stays_put_and_still_reports_true() {
        let mut controller = BeelineController::new(Vector3::new(5.0, 5.0, 5.0));
        let mut pos = Vector3::new(5.0, 5.0, 5.0);
        let mut yaw = 1.25;
        let mut speed = 6.0;

        assert!(controller.next_step(&mut speed, 1.0, &mut pos, &mut yaw));
        assert_eq!(pos, Vector3::new(5.0, 5.0, 5.0));
        assert_eq!(yaw, 1.25);
    }

    /// The factory parse (beeline_controller.cpp:62-76): `"x,y,z"` around
    /// the first and last comma; the starting position is untouched.
    #[test]
    fn the_factory_parses_the_destination_triple() {
        let factory = BeelineControllerFactory;
        let mut start_pos = Vector3::new(99.0, 99.0, 99.0);
        let mut controller = factory
            .create("3.5,1.0,-2.25", 7.0, &mut start_pos)
            .unwrap();

        assert_eq!(start_pos, Vector3::new(99.0, 99.0, 99.0));
        // Drive the parsed destination out through the controller: from
        // (3.5, 1.0, -2.25) the bot is already there and stays put.
        let mut pos = Vector3::new(3.5, 1.0, -2.25);
        let mut yaw = 0.0;
        let mut speed = 6.0;
        assert!(controller.next_step(&mut speed, 1.0, &mut pos, &mut yaw));
        assert_eq!(pos, Vector3::new(3.5, 1.0, -2.25));

        // Spaces around the numbers are tolerated the way `atof` skips
        // leading whitespace: the destination still lands on (10, 0, 0)
        // and the bot strides toward it.
        let mut controller = factory.create("10, 0, 0", 2.0, &mut start_pos).unwrap();
        let mut pos = Vector3::new(4.0, 0.0, 0.0);
        let mut yaw = 0.0;
        let mut speed = 2.0;
        assert!(controller.next_step(&mut speed, 1.0, &mut pos, &mut yaw));
        assert_eq!(pos.x, 6.0);
        assert_eq!(yaw, FRAC_PI_2);
    }

    /// The degenerate parse arms, reproduced from the reference's
    /// `size_t` slicing: one comma wraps the y count to the tail, no
    /// comma wraps both offsets onto the whole string, and a non-numeric
    /// segment lands on `atof`'s 0.0.
    #[test]
    fn degenerate_parses_match_the_reference_slice_shapes() {
        assert_eq!(parse_destination("1,2"), Vector3::new(1.0, 2.0, 2.0));
        assert_eq!(parse_destination("5"), Vector3::new(5.0, 5.0, 5.0));
        assert_eq!(parse_destination(""), Vector3::new(0.0, 0.0, 0.0));
        assert_eq!(parse_destination("x,y,z"), Vector3::new(0.0, 0.0, 0.0));
    }

    /// Registration face: `"Beeline"` resolves through the fleet's
    /// controller registry (`MainApp::addFactory`, main_app.cpp:807-815)
    /// and the created controller walks the same fixed route.
    #[test]
    fn the_registry_creates_a_beeline_controller() {
        let mut registry = ControllerRegistry::new();
        registry.register("Beeline", Box::new(BeelineControllerFactory));

        let mut start_pos = Vector3::new(0.0, 0.0, 0.0);
        let mut controller = registry
            .create("Beeline", "10,0,0", 2.0, &mut start_pos)
            .unwrap()
            .expect("Beeline creates a controller");

        let mut pos = start_pos;
        let mut yaw = 0.0;
        let mut speed = 2.0;
        for _ in 0..5 {
            assert!(controller.next_step(&mut speed, 1.0, &mut pos, &mut yaw));
        }
        assert_eq!(pos, Vector3::new(8.0, 0.0, 0.0));
    }
}
