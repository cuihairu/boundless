//! BW-10-01-05 the 2D line equation, following `lib/math/lineeq.hpp/.ipp`.
//!
//! `LineEq` is an infinite line in the plane of another plane (the way
//! `PlaneEq::intersect` produces it); `HullTree::add` intersects hull border
//! planes pairwise through it. Normals must be normalised for `param` and
//! friends to work, exactly as the reference header warns.

use super::vector::Vector2;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LineEq {
    normal: Vector2,
    d: f32,
}

impl LineEq {
    /// `LineEq(normal, d)` (`lineeq.hpp:19-20`).
    pub fn new(normal: Vector2, d: f32) -> Self {
        Self { normal, d }
    }

    /// `normal()` (`lineeq.hpp:33`).
    pub fn normal(&self) -> &Vector2 {
        &self.normal
    }

    /// `d()` (`lineeq.hpp:34`).
    pub fn d(&self) -> f32 {
        self.d
    }

    /// `intersect` (`lineeq.ipp:22-27`): the distance along this line where
    /// the other crosses it.
    pub fn intersect(&self, other: &LineEq) -> f32 {
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

    /// `project` (`lineeq.ipp:44-48`): the parameter of the point projected
    /// onto the line, along the unit direction `(n.y, -n.x)`.
    pub fn project(&self, point: &Vector2) -> f32 {
        let dir = Vector2::new(self.normal.y, -self.normal.x);
        dir.dot_product(&(*point - self.normal * self.d))
    }

    /// `isMinCutter` (`lineeq.ipp:57-60`): an intersection above the minimum
    /// forms the new minimum when this holds.
    pub fn is_min_cutter(&self, cutter: &LineEq) -> bool {
        self.normal.cross_product(&cutter.normal) > 0.0
    }

    /// `isParallel` (`lineeq.ipp:66-69`).
    pub fn is_parallel(&self, other: &LineEq) -> bool {
        self.normal.cross_product(&other.normal).abs() < 0.001
    }

    /// `isInFrontOf` (`lineeq.ipp:75-78`).
    pub fn is_in_front_of(&self, point: &Vector2) -> bool {
        self.normal.dot_product(point) > self.d
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_almost_eq(a: f32, b: f32) {
        assert!((a - b).abs() < 1e-5, "{a} != {b}");
    }

    /// The x axis (normal +y, d 0) and the y axis (normal +x, d 0) cross at
    /// the origin; each reports the other's crossing at parameter 0.
    #[test]
    fn axes_intersect_at_the_origin() {
        let x_axis = LineEq::new(Vector2::new(0.0, 1.0), 0.0);
        let y_axis = LineEq::new(Vector2::new(1.0, 0.0), 0.0);
        assert_almost_eq(x_axis.intersect(&y_axis), 0.0);
        assert_almost_eq(y_axis.intersect(&x_axis), 0.0);
        // param(0) is the closest point to the origin for a line through it.
        assert_eq!(x_axis.param(0.0), Vector2::ZERO);
    }

    #[test]
    fn param_walks_along_the_line_direction() {
        // Line y = 2: normal (0,1), d 2. The direction is (1,0), so
        // param(t) = (2, t)... pinned against the reference formula:
        // (n.x*d + n.y*t, n.y*d - n.x*t) = (t, 2).
        let line = LineEq::new(Vector2::new(0.0, 1.0), 2.0);
        assert_eq!(line.param(3.0), Vector2::new(3.0, 2.0));
        // project is the signed parameter of a point.
        assert_almost_eq(line.project(&Vector2::new(5.0, 9.0)), 5.0);
        assert_almost_eq(line.project(&Vector2::new(-2.0, 1.0)), -2.0);
    }

    #[test]
    fn cutter_and_parallel_and_front_tests() {
        let x_axis = LineEq::new(Vector2::new(0.0, 1.0), 0.0);
        let up = LineEq::new(Vector2::new(0.0, 1.0), 1.0);
        let down = LineEq::new(Vector2::new(0.0, -1.0), -1.0);
        // Same normal: parallel; opposite normals: parallel too.
        assert!(x_axis.is_parallel(&up));
        assert!(x_axis.is_parallel(&down));
        // A same-facing parallel line in front cuts us off entirely.
        assert!(x_axis.is_in_front_of(&up.param(0.0)));
        assert!(!up.is_in_front_of(&x_axis.param(0.0)));
        // Minimum cutters cross with a positive 2D cross product.
        let left = LineEq::new(Vector2::new(1.0, 0.0), 0.0);
        assert!(left.is_min_cutter(&x_axis));
        assert!(!x_axis.is_min_cutter(&left));
    }
}
