//! BW-10-07-04: `ClosestTriangle` (`common/closest_triangle.hpp`) — the
//! callback `BigWorld.findDropPoint` drives: every hit records the
//! world-transformed triangle and the sweep keeps asking for nearer ones
//! (`COLLIDE_BEFORE`).

use crate::portal::{COLLIDE_BEFORE, CollisionCallback, CollisionObstacle};
use crate::world_triangle::WorldTriangle;
use foundation::math::vector::Vector3;

/// `ClosestTriangle`: records the last (closest) triangle hit, lifted
/// into world space through the obstacle's transform.
pub struct ClosestTriangle {
    /// `tri_`.
    tri: WorldTriangle,
    /// `shouldUseTriangleFlags_` — the ctor flag (`true` keeps the hit's
    /// flags, `false` zeroes them).
    should_use_triangle_flags: bool,
}

impl ClosestTriangle {
    /// `ClosestTriangle(true)` — the default-constructed probe.
    pub fn new() -> Self {
        Self {
            tri: WorldTriangle::new(Vector3::ZERO, Vector3::ZERO, Vector3::ZERO, 0),
            should_use_triangle_flags: true,
        }
    }

    /// `ClosestTriangle(shouldUseTriangleFlags)`.
    pub fn with_flags(should_use_triangle_flags: bool) -> Self {
        Self {
            tri: WorldTriangle::new(Vector3::ZERO, Vector3::ZERO, Vector3::ZERO, 0),
            should_use_triangle_flags,
        }
    }

    /// `triangle()` — the closest triangle so far.
    pub fn triangle(&self) -> &WorldTriangle {
        &self.tri
    }
}

impl Default for ClosestTriangle {
    fn default() -> Self {
        Self::new()
    }
}

impl CollisionCallback for ClosestTriangle {
    /// `operator()` (`closest_triangle.hpp:23-44`): store the triangle
    /// in world coordinates, return COLLIDE_BEFORE so the sweep narrows
    /// onto the nearest hit.
    fn visit_deprecated(
        &mut self,
        obstacle: &CollisionObstacle,
        triangle: &WorldTriangle,
        _dist: f32,
    ) -> i32 {
        self.tri = WorldTriangle::new(
            obstacle.transform.apply_point(&triangle.v0()),
            obstacle.transform.apply_point(&triangle.v1()),
            obstacle.transform.apply_point(&triangle.v2()),
            if self.should_use_triangle_flags {
                triangle.flags()
            } else {
                0
            },
        );

        COLLIDE_BEFORE
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::portal::COLLIDE_ALL;
    use crate::world_triangle::{TRIANGLE_TERRAIN, TRIANGLE_WATER};
    use foundation::math::matrix::Matrix;

    fn v(x: f32, y: f32, z: f32) -> Vector3 {
        Vector3::new(x, y, z)
    }

    fn ob(transform: &Matrix) -> CollisionObstacle {
        let mut inverse = *transform;
        inverse.invert();
        CollisionObstacle::new(transform, &inverse)
    }

    #[test]
    fn the_default_probe_records_the_world_triangle() {
        let mut fdpt = ClosestTriangle::new();
        let identity = Matrix::identity();
        let handle = ob(&identity);
        let tri = WorldTriangle::new(v(0.0, 0.0, 5.0), v(10.0, 0.0, 5.0), v(0.0, 10.0, 5.0), 0);
        assert_eq!(fdpt.visit(&handle, &tri, 5.0), COLLIDE_BEFORE);

        let recorded = fdpt.triangle();
        assert_eq!(recorded.v0(), v(0.0, 0.0, 5.0));
        assert_eq!(recorded.v1(), v(10.0, 0.0, 5.0));
        assert_eq!(recorded.v2(), v(0.0, 10.0, 5.0));
        assert_eq!(recorded.flags(), 0);
    }

    #[test]
    fn the_probe_lifts_the_hit_through_the_transform() {
        let mut fdpt = ClosestTriangle::new();
        let mut transform = Matrix::identity();
        transform.set_translate(10.0, 0.0, 0.0);
        let handle = ob(&transform);
        let tri = WorldTriangle::new(v(0.0, 0.0, 5.0), v(10.0, 0.0, 5.0), v(0.0, 10.0, 5.0), 0);

        assert_eq!(fdpt.visit(&handle, &tri, 5.0), COLLIDE_BEFORE);
        let recorded = fdpt.triangle();
        assert_eq!(recorded.v0(), v(10.0, 0.0, 5.0));
        assert_eq!(recorded.v1(), v(20.0, 0.0, 5.0));
        assert_eq!(recorded.v2(), v(10.0, 10.0, 5.0));
    }

    #[test]
    fn the_flags_flag_decides_what_flags_are_kept() {
        // a dry flag word (water would short-circuit in visit()'s
        // deprecated arm before the recorder sees it)
        let mut t = WorldTriangle::new(v(0.0, 0.0, 5.0), v(1.0, 0.0, 5.0), v(0.0, 1.0, 5.0), 0);
        t.set_flags(TRIANGLE_TERRAIN);
        let identity = Matrix::identity();
        let handle = ob(&identity);

        // the default keeps the flags
        let mut fdpt = ClosestTriangle::new();
        assert_eq!(fdpt.visit(&handle, &t, 1.0), COLLIDE_BEFORE);
        assert_eq!(fdpt.triangle().flags(), TRIANGLE_TERRAIN);

        // with_flags(false) zeroes them
        let mut fdpt = ClosestTriangle::with_flags(false);
        assert_eq!(fdpt.visit(&handle, &t, 1.0), COLLIDE_BEFORE);
        assert_eq!(fdpt.triangle().flags(), 0);
    }

    #[test]
    fn the_water_arm_short_circuits_before_the_recorder() {
        // doVisit stays default (not implemented), so visit() answers the
        // water triangle itself — COLLIDE_ALL, no recording — and only the
        // dry triangle reaches operator() (the deprecated hook).
        let mut t = WorldTriangle::new(v(0.0, 0.0, 5.0), v(1.0, 0.0, 5.0), v(0.0, 1.0, 5.0), 0);
        t.set_flags(TRIANGLE_WATER);
        let identity = Matrix::identity();
        let handle = ob(&identity);

        let mut fdpt = ClosestTriangle::new();
        assert_eq!(fdpt.visit(&handle, &t, 1.0), COLLIDE_ALL);
        // nothing was recorded by the short circuit
        assert_eq!(fdpt.triangle().v0(), Vector3::ZERO);

        let dry = {
            let mut d = t;
            d.set_flags(0);
            d
        };
        assert_eq!(fdpt.visit(&handle, &dry, 1.0), COLLIDE_BEFORE);
    }
}
