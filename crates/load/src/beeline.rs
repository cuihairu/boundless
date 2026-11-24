//! BW-17-01-04: the Beeline movement controller
//! (`server/tools/bots/beeline_controller.{hpp,cpp}`).
//!
//! The reference controller walks a straight line to a fixed
//! destination: every step covers `speed * dTime` along the direction to
//! `destinationPos_`, turning the yaw to face it
//! (`beeline_controller.cpp:26-45`). It never reports completion — the
//! `true` return is unconditional, and a final step shorter than the
//! tick's travel is simply not taken (the position holds, the yaw keeps
//! its last value); arrival clearing is the caller's business
//! (`ClientApp::moveTo`, BW-17-01-03).
//!
//! The factory (`beeline_controller.cpp:57-81`) is registered as
//! `"Beeline"` and reads its data as `"x,y,z"` with the C `atof`
//! tolerance — missing or unparsable parts are `0.0`.

use crate::bot::{MovementController, MovementFactory};
use foundation::math::vector::Vector3;

/// `Beeline::BeelineController`: the destination is the whole state.
pub struct BeelineController {
    destination_pos: Vector3,
}

impl BeelineController {
    /// The reference constructor (`beeline_controller.cpp:21-24`).
    pub fn new(destination_pos: Vector3) -> Self {
        Self { destination_pos }
    }
}

impl MovementController for BeelineController {
    fn next_step(&mut self, speed: &mut f32, dtime: f32, pos: &mut Vector3, yaw: &mut f32) -> bool {
        // Distance travelled per tick
        let distance = *speed * dtime;

        // Vector to dest and zzp respectively
        let dest_vec = self.destination_pos - *pos;
        let length = dest_vec.length();

        // Move closer if we are aren't there yet
        if length > distance {
            *pos = *pos + dest_vec * (distance / length);
            *yaw = dest_vec.yaw();
        }

        true
    }
}

/// `atof` (`cstdmf`): the longest numeric prefix, `0.0` when there is
/// none.
fn c_atof(text: &str) -> f32 {
    let bytes = text.as_bytes();
    let mut end = 0;
    // Leading whitespace, then an optional sign, then digits with at most
    // one dot, then an optional exponent.
    let mut i = 0;
    while i < bytes.len() && bytes[i].is_ascii_whitespace() {
        i += 1;
    }
    let start = i;
    if i < bytes.len() && (bytes[i] == b'+' || bytes[i] == b'-') {
        i += 1;
    }
    let digits_start = i;
    let mut seen_dot = false;
    while i < bytes.len() {
        let b = bytes[i];
        if b.is_ascii_digit() {
            i += 1;
        } else if b == b'.' && !seen_dot {
            seen_dot = true;
            i += 1;
        } else {
            break;
        }
    }
    if i > digits_start {
        end = i;
        // Optional exponent: e/E, optional sign, at least one digit.
        let j = i;
        if j < bytes.len() && (bytes[j] == b'e' || bytes[j] == b'E') {
            let mut k = j + 1;
            if k < bytes.len() && (bytes[k] == b'+' || bytes[k] == b'-') {
                k += 1;
            }
            let exp_start = k;
            while k < bytes.len() && bytes[k].is_ascii_digit() {
                k += 1;
            }
            if k > exp_start {
                end = k;
            }
        }
    }
    text[start..end].parse::<f32>().unwrap_or(0.0)
}

/// `Beeline::BeelineControllerFactory`, registered as `"Beeline"`
/// (`beeline_controller.cpp:51-83`).
pub struct BeelineFactory;

impl MovementFactory for BeelineFactory {
    fn create(
        &self,
        data: &str,
        _speed: f32,
        _start_pos: &mut Vector3,
    ) -> Option<Box<dyn MovementController>> {
        // The reference splits at the first and last comma
        // (`find_first_of` / `find_last_of`, :71-75). A data string
        // without both separators relies on `npos` arithmetic in the
        // reference; the port reads that as the missing parts being 0.0.
        let com1 = data.find(',').unwrap_or(data.len());
        let com2 = data.rfind(',').unwrap_or(data.len()).max(com1);
        let destination = Vector3::new(
            c_atof(&data[..com1]),
            c_atof(&data[(com1 + 1).min(com2)..com2]),
            c_atof(&data[(com2 + 1).min(data.len())..]),
        );
        Some(Box::new(BeelineController::new(destination)))
    }
}

#[cfg(test)]
mod tests {
    use crate::fleet::ControllerRegistry;

    use super::*;

    /// Compares with a tolerance finer than one step.
    fn near(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-4
    }

    fn near_pos(a: Vector3, b: Vector3) -> bool {
        near(a.x, b.x) && near(a.y, b.y) && near(a.z, b.z)
    }

    #[test]
    fn each_step_advances_by_speed_times_dtime() {
        // The fixed route: a straight line toward +x, one 0.6 step per
        // tick, the yaw facing the destination (beeline_controller.cpp:31-42).
        let mut controller = BeelineController::new(Vector3::new(100.0, 0.0, 0.0));
        let mut speed = 6.0;
        let mut pos = Vector3::new(0.0, 0.0, 0.0);
        let mut yaw = 0.0;

        for expected in [0.6, 1.2, 1.8] {
            assert!(controller.next_step(&mut speed, 0.1, &mut pos, &mut yaw));
            assert!(near(pos.x, expected), "x {}", pos.x);
            assert!(near(pos.y, 0.0) && near(pos.z, 0.0));
        }
        // atan2(x, z) of the travel vector: due +x is a quarter turn.
        assert!(near(yaw, std::f32::consts::FRAC_PI_2), "yaw {yaw}");
        // A controller may slow the bot down; the beeline never does.
        assert!(near(speed, 6.0));
    }

    #[test]
    fn a_short_final_step_is_not_taken() {
        // With 1.0 to go and 0.6 of travel, the second step holds the
        // position and the yaw keeps the value from the last moving step
        // (`length > distance` is false, :38).
        let mut controller = BeelineController::new(Vector3::new(1.0, 0.0, 0.0));
        let mut speed = 6.0;
        let mut pos = Vector3::new(0.0, 0.0, 0.0);
        let mut yaw = 0.0;

        assert!(controller.next_step(&mut speed, 0.1, &mut pos, &mut yaw));
        assert!(near(pos.x, 0.6));

        let (held_pos, held_yaw) = (pos, yaw);
        assert!(controller.next_step(&mut speed, 0.1, &mut pos, &mut yaw));
        assert!(near_pos(pos, held_pos), "holds at {pos:?}");
        assert!(near(yaw, held_yaw));
        // And it never reports completion (`return true`, :44) — arrival
        // clearing belongs to ClientApp::moveTo.
        assert!(controller.next_step(&mut speed, 0.1, &mut pos, &mut yaw));
        assert!(near_pos(pos, held_pos));
    }

    #[test]
    fn the_diagonal_route_steps_along_the_unit_direction() {
        // Destination off-axis: each step is `dest * 0.6 / |dest|` and
        // the yaw faces it (atan2(x, z)).
        let mut controller = BeelineController::new(Vector3::new(10.5, 20.0, 30.0));
        let mut speed = 6.0;
        let mut pos = Vector3::new(0.0, 0.0, 0.0);
        let mut yaw = 0.0;

        assert!(controller.next_step(&mut speed, 0.1, &mut pos, &mut yaw));
        let length = (10.5_f32 * 10.5 + 20.0 * 20.0 + 30.0 * 30.0).sqrt();
        assert!(near_pos(
            pos,
            Vector3::new(10.5, 20.0, 30.0) * (0.6 / length)
        ));
        assert!(near(yaw, (Vector3::new(10.5, 0.0, 30.0)).yaw()));
    }

    #[test]
    fn the_factory_builds_from_the_comma_data() {
        // `"x,y,z"` via the first/last comma split (:71-75); a part that
        // is not a number reads as 0.0, as `atof` does.
        let mut pos = Vector3::new(0.0, 0.0, 0.0);
        let mut controller = BeelineFactory
            .create("10.5,20,30", 6.0, &mut pos)
            .expect("the factory always builds");
        let mut speed = 6.0;
        let mut yaw = 0.0;
        assert!(controller.next_step(&mut speed, 0.1, &mut pos, &mut yaw));
        let length = (10.5_f32 * 10.5 + 20.0 * 20.0 + 30.0 * 30.0).sqrt();
        assert!(near_pos(
            pos,
            Vector3::new(10.5, 20.0, 30.0) * (0.6 / length)
        ));

        let mut pos = Vector3::new(0.0, 0.0, 0.0);
        let mut controller = BeelineFactory.create("5,ignored,", 6.0, &mut pos).unwrap();
        assert!(controller.next_step(&mut speed, 0.1, &mut pos, &mut yaw));
        assert!(near(pos.x, 0.6) && near(pos.y, 0.0) && near(pos.z, 0.0));
    }

    #[test]
    fn atof_takes_the_longest_numeric_prefix() {
        assert!(near(c_atof("10.5"), 10.5));
        assert!(near(c_atof(" 7x"), 7.0));
        assert!(near(c_atof("-2.5,"), -2.5));
        assert!(near(c_atof("1e2"), 100.0));
        assert!(near(c_atof("1e-2"), 0.01));
        assert!(near(c_atof("7e"), 7.0));
        assert!(near(c_atof(""), 0.0));
        assert!(near(c_atof("abc"), 0.0));
        assert!(near(c_atof("1.2.3"), 1.2));
    }

    #[test]
    fn the_registry_drives_the_fixed_route() {
        // The end-to-end face `MainApp::createMovementController` uses:
        // register "Beeline", resolve it through the fleet's registry and
        // walk the same fixed route (main_app.cpp:740-803).
        let mut registry = ControllerRegistry::new();
        registry.register("Beeline", Box::new(BeelineFactory));

        let mut pos = Vector3::new(0.0, 0.0, 0.0);
        let mut controller = registry
            .create("Beeline", "100,0,0", 6.0, &mut pos)
            .expect("registered")
            .expect("the factory always builds");
        let mut speed = 6.0;
        let mut yaw = 0.0;
        for expected in [0.6, 1.2, 1.8] {
            controller.next_step(&mut speed, 0.1, &mut pos, &mut yaw);
            assert!(near(pos.x, expected));
        }
    }
}
