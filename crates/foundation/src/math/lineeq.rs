//! BW-10-01-05 a 2D line equation, following the reference in
//! `lib/math/lineeq.{hpp,ipp}`.
//!
//! The line is an infinite line on some plane, not a segment or a ray:
//! `normal · p = d` with the normal on the plane. The reference notes that
//! `param` and friends only work when the normals are normalised, so this
//! port normalises in the two-point constructor like the reference does.

use super::vector::Vector2;

/// `LineEq` (`lineeq.hpp:14-37`): an infinite line on a plane.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LineEq {
    normal: Vector2,
    d: f32,
}

impl LineEq {
    /// `LineEq(normal, d)` (`lineeq.hpp:19-21`): `normal · p = d`.
    pub fn new(normal: Vector2, d: f32) -> Self {
        Self { normal, d }
    }

    /// `LineEq(v0, v1, needToNormalise)` (`lineeq.ipp:6-15`): the normal is
    /// `(v0.y - v1.y, v1.x - v0.x)`, normalised when asked, and
    /// `d = normal · v0`.
    pub fn from_points(v0: &Vector2, v1: &Vector2, need_to_normalise: bool) -> Self {
        let mut normal = Vector2::new(v0.y - v1.y, v1.x - v0.x);
        if need_to_normalise {
            normal.normalise();
        }
        let d = normal.dot_product(v0);
        Self { normal, d }
    }

    /// `intersect` (`lineeq.ipp:22-26`): the distance along this line at
    /// which it crosses `other`.
    pub fn intersect(&self, other: &Self) -> f32 {
        let on = other.normal;
        (other.d - self.d * on.dot_product(&self.normal)) / on.cross_product(&self.normal)
    }

    /// `param` (`lineeq.ipp:33-38`): the point at the given parameter.
    pub fn param(&self, t: f32) -> Vector2 {
        Vector2::new(
            self.normal.x * self.d + self.normal.y * t,
            self.normal.y * self.d - self.normal.x * t,
        )
    }

    /// `project` (`lineeq.ipp:44-47`): the parameter of the given point
    /// projected onto the line.
    pub fn project(&self, point: &Vector2) -> f32 {
        // the tangent direction, also unit length
        let dir = Vector2::new(self.normal.y, -self.normal.x);
        dir.dot_product(&Vector2::new(
            point.x - self.normal.x * self.d,
            point.y - self.normal.y * self.d,
        ))
    }

    /// `isMinCutter` (`lineeq.ipp:57-59`).
    pub fn is_min_cutter(&self, cutter: &Self) -> bool {
        self.normal.cross_product(&cutter.normal) > 0.0
    }

    /// `isParallel` (`lineeq.ipp:66-68`).
    pub fn is_parallel(&self, other: &Self) -> bool {
        (self.normal.cross_product(&other.normal)).abs() < 0.001
    }

    /// `isInFrontOf` (`lineeq.ipp:75-77`).
    pub fn is_in_front_of(&self, point: &Vector2) -> bool {
        self.normal.dot_product(point) > self.d
    }

    /// `normal()` (`lineeq.hpp:33-34`).
    pub fn normal(&self) -> Vector2 {
        self.normal
    }

    /// `d()` (`lineeq.hpp:35-36`).
    pub fn d(&self) -> f32 {
        self.d
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_keeps_the_normal_and_offset() {
        let line = LineEq::new(Vector2::new(0.0, 1.0), 4.0);
        assert_eq!(line.normal(), Vector2::new(0.0, 1.0));
        assert_eq!(line.d(), 4.0);
    }

    #[test]
    fn from_points_builds_the_left_hand_normal() {
        // From (0,0) to (4,0): the reference normal is (0-0, 4-0) = (0,4),
        // normalised (0,1), d = 0.
        let line = LineEq::from_points(&Vector2::new(0.0, 0.0), &Vector2::new(4.0, 0.0), true);
        assert_eq!(line.normal(), Vector2::new(0.0, 1.0));
        assert_eq!(line.d(), 0.0);
    }

    #[test]
    fn from_points_without_normalising_keeps_the_raw_normal() {
        // Unnormalised the same pair keeps (0,4) and d stays 0.
        let line = LineEq::from_points(&Vector2::new(0.0, 0.0), &Vector2::new(4.0, 0.0), false);
        assert_eq!(line.normal(), Vector2::new(0.0, 4.0));
    }

    #[test]
    fn intersect_measures_along_this_line() {
        // y = 2 crossed by x = 3: the crossing is (3,2), parameter 3 along
        // the horizontal line.
        let horizontal =
            LineEq::from_points(&Vector2::new(0.0, 2.0), &Vector2::new(5.0, 2.0), true);
        let vertical = LineEq::from_points(&Vector2::new(3.0, 0.0), &Vector2::new(3.0, 5.0), true);
        assert!((horizontal.intersect(&vertical) - 3.0).abs() < 1e-5);
    }

    #[test]
    fn param_and_project_round_trip() {
        let line = LineEq::from_points(&Vector2::new(1.0, 2.0), &Vector2::new(4.0, 2.0), true);
        let point = line.param(7.0);
        assert!((line.project(&point) - 7.0).abs() < 1e-5);
        // A point off the line projects to its parameter along it.
        let off = Vector2::new(point.x, 99.0);
        assert!((line.project(&off) - line.project(&point)).abs() < 1e-5);
    }

    #[test]
    fn the_cutters_and_the_parallel_test() {
        let horizontal = LineEq::new(Vector2::new(0.0, 1.0), 0.0);
        let vertical = LineEq::new(Vector2::new(1.0, 0.0), 0.0);
        // n × cn > 0 only for the vertical's normal against the horizontal
        assert!(vertical.is_min_cutter(&horizontal));
        assert!(!horizontal.is_min_cutter(&vertical));
        assert!(horizontal.is_parallel(&LineEq::new(Vector2::new(0.0, -1.0), 5.0)));
        assert!(!horizontal.is_parallel(&vertical));
    }

    #[test]
    fn is_in_front_of_signs_the_sides() {
        // y = 4: the normal points up, so higher points are in front.
        let line = LineEq::new(Vector2::new(0.0, 1.0), 4.0);
        assert!(line.is_in_front_of(&Vector2::new(10.0, 5.0)));
        assert!(!line.is_in_front_of(&Vector2::new(10.0, 3.0)));
        assert!(!line.is_in_front_of(&Vector2::new(10.0, 4.0)));
    }
}
