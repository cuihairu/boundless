//! BW-01-01-04 planes, following the reference scalar path in
//! `lib/math/planeeq.ipp` / `planeeq.cpp`.
//!
//! The plane equation is `normal · p = d`; after a normalised `init` the
//! normal is unit length and `d` is the signed offset from the origin.

use super::vector::Vector3;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PlaneEq {
    normal: Vector3,
    d: f32,
}

impl Default for PlaneEq {
    /// The reference default constructor leaves the fields uninitialised
    /// (`planeeq.ipp:25-30`); the port zeroes them so the type is total.
    fn default() -> Self {
        Self {
            normal: Vector3::new(0.0, 0.0, 0.0),
            d: 0.0,
        }
    }
}

impl PlaneEq {
    /// `PlaneEq(normal, d)` / `init(normal, d)` (`planeeq.ipp:29-51`): a plane
    /// from a normal and the dot product of that normal with any point on the
    /// plane, so `normal · p = d`. The normal is not normalised here; callers
    /// that build a plane from a raw `(normal, d)` pair normalise both like
    /// the reference `ChunkBoundary` constructor (`chunk_boundary.cpp:72-77`).
    pub fn new(normal: Vector3, d: f32) -> Self {
        Self { normal, d }
    }

    /// `normal()` (`planeeq.ipp:173-177`): unit length after a normalised
    /// init, pointing out of the front of the plane.
    pub fn normal(&self) -> &Vector3 {
        &self.normal
    }

    /// `d()` (`planeeq.ipp:179-183`): the dot product of the normal with any
    /// point on the plane.
    pub fn d(&self) -> f32 {
        self.d
    }

    /// `init(p0, p1, p2)` (`planeeq.ipp:157-171`): the normal is
    /// `(p1-p0) × (p2-p0)`, normalised; `d = normal · p0`. The points must be
    /// clockwise seen from the front and must not be colinear.
    pub fn init(&mut self, p0: &Vector3, p1: &Vector3, p2: &Vector3) {
        let e0 = *p1 - *p0;
        let e1 = *p2 - *p0;
        let mut normal = self.normal.cross_product(&e0, &e1);
        normal.normalise();
        self.normal = normal;
        self.d = normal.dot_product(p0);
    }

    /// `intersectRay` (`planeeq.ipp:245-251`): the hit point of a ray from
    /// `source` along `dir`.
    pub fn intersect_ray(&self, source: &Vector3, dir: &Vector3) -> Vector3 {
        *source + *dir * self.intersect_ray_half(source, self.normal.dot_product(dir))
    }

    /// `intersectRayHalf` (`planeeq.ipp:253-260`): the ray parameter the plane
    /// is hit at; the reference's `isZero` makes a parallel ray report a miss
    /// by returning 0.
    pub fn intersect_ray_half(&self, source: &Vector3, normal_dot_dir: f32) -> f32 {
        if normal_dot_dir == 0.0 {
            0.0
        } else {
            (self.d - self.normal.dot_product(source)) / normal_dot_dir
        }
    }

    /// `distanceTo` (`planeeq.ipp:213-217`): the signed distance; a point
    /// behind the plane reports a negative value.
    pub fn distance_to(&self, point: &Vector3) -> f32 {
        self.normal.dot_product(point) - self.d
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_almost_eq(a: f32, b: f32) {
        assert!((a - b).abs() < 1e-6, "{a} != {b}");
    }

    #[test]
    fn new_builds_a_plane_from_normal_and_offset() {
        let plane = PlaneEq::new(Vector3::new(0.0, 0.0, 1.0), 5.0);
        assert_eq!(plane.normal().z, 1.0);
        assert_eq!(plane.d(), 5.0);
        assert_almost_eq(plane.distance_to(&Vector3::new(0.0, 0.0, 7.0)), 2.0);
    }

    #[test]
    fn init_normalises_the_crossed_normal() {
        let mut plane = PlaneEq::default();
        // A slanted triangle whose cross product is (7, 14, 21) pre-scale.
        plane.init(
            &Vector3::new(0.0, 0.0, 0.0),
            &Vector3::new(2.0, 0.0, 0.0),
            &Vector3::new(0.0, 3.0, 0.0),
        );
        // cross((2,0,0), (0,3,0)) = (0, 0, 6), normalised (0, 0, 1).
        assert_almost_eq(plane.normal().z, 1.0);
        assert_almost_eq(plane.normal().x, 0.0);
        assert_almost_eq(plane.normal().y, 0.0);
        // The plane through the origin: d = normal . p0 = 0.
        assert_almost_eq(plane.d(), 0.0);
    }

    #[test]
    fn distance_to_signs_the_sides() {
        let mut plane = PlaneEq::default();
        plane.init(
            &Vector3::new(0.0, 0.0, 0.0),
            &Vector3::new(1.0, 0.0, 0.0),
            &Vector3::new(0.0, 1.0, 0.0),
        );
        assert_almost_eq(plane.distance_to(&Vector3::new(0.0, 0.0, 5.0)), 5.0);
        assert_almost_eq(plane.distance_to(&Vector3::new(0.0, 0.0, -2.0)), -2.0);
    }

    #[test]
    fn intersect_ray_hits_the_plane() {
        let mut plane = PlaneEq::default();
        plane.init(
            &Vector3::new(0.0, 0.0, 0.0),
            &Vector3::new(1.0, 0.0, 0.0),
            &Vector3::new(0.0, 1.0, 0.0),
        );
        let hit = plane.intersect_ray(&Vector3::new(0.0, 0.0, 2.0), &Vector3::new(0.0, 0.0, -1.0));
        assert_almost_eq(hit.x, 0.0);
        assert_almost_eq(hit.y, 0.0);
        assert_almost_eq(hit.z, 0.0);

        let off = plane.intersect_ray(&Vector3::new(1.0, 1.0, 4.0), &Vector3::new(0.0, 0.0, -2.0));
        assert_almost_eq(off.x, 1.0);
        assert_almost_eq(off.y, 1.0);
        assert_almost_eq(off.z, 0.0);
    }

    #[test]
    fn parallel_ray_reports_no_hit() {
        let mut plane = PlaneEq::default();
        plane.init(
            &Vector3::new(0.0, 0.0, 0.0),
            &Vector3::new(1.0, 0.0, 0.0),
            &Vector3::new(0.0, 1.0, 0.0),
        );
        // normal . dir == 0: the half parameter collapses to 0 and the
        // intersection lands back on the source.
        let hit = plane.intersect_ray(&Vector3::new(3.0, 4.0, 5.0), &Vector3::new(1.0, 0.0, 0.0));
        assert_almost_eq(hit.x, 3.0);
        assert_almost_eq(hit.y, 4.0);
        assert_almost_eq(hit.z, 5.0);
    }
}
