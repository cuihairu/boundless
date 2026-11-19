//! BW-01-01-04 angle normalisation (`lib/math/angle.ipp`).

/// `MATH_PI` in the reference.
pub const MATH_PI: f32 = std::f32::consts::PI;

/// Normalise to `(-π, π]` following the reference branch order
/// (`angle.ipp:124-157`): the boundary `−π` folds to `+π`.
pub fn normalise(value: f32) -> f32 {
    let three_pi = MATH_PI * 3.0;
    let two_pi = MATH_PI * 2.0;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_in_range_pass_through() {
        assert_eq!(normalise(0.0), 0.0);
        assert_eq!(normalise(MATH_PI), MATH_PI, "π itself stays");
        assert_eq!(normalise(1.5), 1.5);
        assert_eq!(normalise(-1.5), -1.5);
    }

    #[test]
    fn minus_pi_folds_to_plus_pi() {
        let result = normalise(-MATH_PI);
        assert!(result > 0.0, "−π must fold upward, got {result}");
        assert!((result - MATH_PI).abs() < 1e-6);
    }

    #[test]
    fn small_wrap() {
        let value = MATH_PI + 0.5;
        assert!((normalise(value) - (-MATH_PI + 0.5)).abs() < 1e-5);
        let value = -MATH_PI - 0.5;
        assert!((normalise(value) - (MATH_PI - 0.5)).abs() < 1e-5);
    }

    #[test]
    fn large_wrap_uses_floor_branch() {
        // Use non-integer multiples so tiny f32 error cannot flip the floor.
        let result = normalise(MATH_PI * 5.0 + 0.3);
        assert!((result - (0.3 - MATH_PI)).abs() < 1e-4, "got {result}");
        let result = normalise(-MATH_PI * 3.0 - 0.2);
        assert!((result - (MATH_PI - 0.2)).abs() < 1e-4, "got {result}");
    }
}
