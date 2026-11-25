//! BW-13-05-03 the line in 3D space (`lib/math/lineeq3.{hpp,ipp}`).
//!
//! Unlike the 2D `LineEq`, a `LineEq3` is an infinite line in 3D space
//! (not a segment or a ray), defined by a point and a direction.

use super::vector::Vector3;

/// `LineEq3` (`lineeq3.hpp:18-39`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LineEq3 {
    point: Vector3,
    direction: Vector3,
    is_valid: bool,
}

impl Default for LineEq3 {
    /// The default constructor leaves the fields at zero/false; the
    /// reference leaves them uninitialised (`lineeq3.ipp:8-13`), so the
    /// port zeroes them to keep the type total.
    fn default() -> Self {
        Self {
            point: Vector3::new(0.0, 0.0, 0.0),
            direction: Vector3::new(1.0, 0.0, 0.0),
            is_valid: false,
        }
    }
}

impl LineEq3 {
    /// `LineEq3(point1, point2)` (`lineeq3.ipp:19-30`): the direction is
    /// `point2 - point1`. Coincident points invalidate the line (the
    /// reference logs `ERROR_MSG` and resets the direction to `(1,0,0)`).
    pub fn from_points(point1: &Vector3, point2: &Vector3) -> Self {
        let direction = *point2 - *point1;
        let mut line = Self {
            point: *point1,
            direction,
            is_valid: true,
        };
        if line.direction.length() == 0.0 {
            line.is_valid = false;
            line.direction = Vector3::new(1.0, 0.0, 0.0);
        }
        line
    }

    /// `isValid()`.
    pub fn is_valid(&self) -> bool {
        self.is_valid
    }

    /// `isValid(bool)`.
    pub fn set_valid(&mut self, value: bool) {
        self.is_valid = value;
    }

    /// `perpDistance(point)` (`lineeq3.ipp:49-54`): the length of the
    /// perpendicular from the point to the line —
    /// `|direction × (point_ − point)| / |direction|`.
    pub fn perp_distance(&self, point: &Vector3) -> f32 {
        let diff = self.point - *point;
        self.direction
            .cross_product(&self.direction, &diff)
            .length()
            / self.direction.length()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_line_is_invalid_with_axis_direction() {
        let line = LineEq3::default();
        assert!(!line.is_valid());
        assert_eq!(line.direction, Vector3::new(1.0, 0.0, 0.0));
        assert_eq!(line.point, Vector3::new(0.0, 0.0, 0.0));
    }

    #[test]
    fn line_from_two_points_carries_point_and_direction() {
        let line = LineEq3::from_points(&Vector3::new(1.0, 2.0, 3.0), &Vector3::new(1.0, 5.0, 3.0));
        assert!(line.is_valid());
        assert_eq!(line.point, Vector3::new(1.0, 2.0, 3.0));
        assert_eq!(line.direction, Vector3::new(0.0, 3.0, 0.0));
    }

    #[test]
    fn coincident_points_invalidate_the_line() {
        let line = LineEq3::from_points(&Vector3::new(4.0, 4.0, 4.0), &Vector3::new(4.0, 4.0, 4.0));
        assert!(!line.is_valid());
        // The reference resets the direction to the x axis on error.
        assert_eq!(line.direction, Vector3::new(1.0, 0.0, 0.0));
        assert_eq!(line.point, Vector3::new(4.0, 4.0, 4.0));
    }

    #[test]
    fn perp_distance_is_the_distance_to_the_infinite_line() {
        let line = LineEq3::from_points(&Vector3::new(0.0, 0.0, 0.0), &Vector3::new(1.0, 0.0, 0.0));
        assert_eq!(line.perp_distance(&Vector3::new(0.5, 0.0, 0.0)), 0.0);
        assert_eq!(line.perp_distance(&Vector3::new(0.5, 3.0, 0.0)), 3.0);
        assert_eq!(line.perp_distance(&Vector3::new(0.5, 0.0, -2.0)), 2.0);
    }

    #[test]
    fn set_valid_flips_the_state() {
        let mut line = LineEq3::default();
        line.set_valid(true);
        assert!(line.is_valid());
        line.set_valid(false);
        assert!(!line.is_valid());
    }
}
