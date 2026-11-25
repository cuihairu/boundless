//! BW-01-01-04 planes, following the reference scalar path in
//! `lib/math/planeeq.ipp` / `planeeq.cpp`.
//!
//! The plane equation is `normal · p = d`; after a normalised `init` the
//! normal is unit length and `d` is the signed offset from the origin.

use super::lineeq::LineEq;
use super::vector::{Vector2, Vector3, almost_equal, almost_zero};

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

    /// `isInFrontOf` (`planeeq.ipp:196-202`): a point within the
    /// `almostEqual` buffer of the plane counts as in front — this is the
    /// sidedness test `HullTree::testPoint` walks its tree with.
    pub fn is_in_front_of(&self, point: &Vector3) -> bool {
        let dot = self.normal.dot_product(point);
        if almost_equal(dot, self.d) {
            return true;
        }
        dot > self.d
    }

    /// `basis` (`planeeq.cpp:85-115`): an orthonormal `(xdir, ydir)` pair
    /// spanning the plane, picked on the normal's largest axis.
    pub fn basis(&self) -> (Vector3, Vector3) {
        const BI_MORE_L: [usize; 3] = [1, 2, 0];
        const BI_LESS_L: [usize; 3] = [2, 0, 1];
        let fabses = [
            self.normal.x.abs(),
            self.normal.y.abs(),
            self.normal.z.abs(),
        ];
        let bi = if fabses[0] > fabses[1] {
            if fabses[0] > fabses[2] { 0 } else { 2 }
        } else if fabses[1] > fabses[2] {
            1
        } else {
            2
        };
        let bi_more = BI_MORE_L[bi];
        let bi_less = BI_LESS_L[bi];

        let mut xdir = Vector3::ZERO;
        xdir.set(bi_less, 0.0);
        xdir.set(bi_more, 1.0);
        xdir.set(bi, -self.normal.get(bi_more) / self.normal.get(bi));

        // The y direction is the cross of the two; both end unit length.
        let mut ydir = Vector3::ZERO;
        ydir = ydir.cross_product(&self.normal, &xdir);
        xdir.normalise();
        ydir.normalise();
        (xdir, ydir)
    }

    /// `param` (`planeeq.cpp:122-133`): the 3D point of a 2D parameter in the
    /// plane's basis, from the plane origin `normal * d / |normal|²`.
    pub fn param(&self, param: Vector2) -> Vector3 {
        let (xdir, ydir) = self.basis();
        let d_over_norm_len = self.d / self.normal.length_squared();
        self.normal * d_over_norm_len + xdir * param.x + ydir * param.y
    }

    /// `project` (`planeeq.cpp:139-152`): the 2D parameter of a 3D point in
    /// the plane's basis.
    pub fn project(&self, point: &Vector3) -> Vector2 {
        let (xdir, ydir) = self.basis();
        let offset = *point - self.normal * (self.d / self.normal.length_squared());
        Vector2::new(xdir.dot_product(&offset), ydir.dot_product(&offset))
    }

    /// `intersect` (`planeeq.cpp:24-76`): this line, defined in this plane,
    /// where the `slice` plane cuts it. A parallel slice gives the zero
    /// line (`LineEq` with zero normal), which `HullTree::add` filters.
    /// The reference's debug reconstitution block prints only; it has no
    /// result-bearing code and is not ported.
    pub fn intersect(&self, slice: &PlaneEq) -> LineEq {
        let dir3d = Vector3::ZERO.cross_product(&self.normal, slice.normal());
        if almost_zero(dir3d.length_squared(), 0.0001) {
            return LineEq::new(Vector2::ZERO, 0.0);
        }
        let normal3d = Vector3::ZERO.cross_product(&self.normal, &dir3d);
        let origin3d = self.param(Vector2::new(0.0, 0.0));
        let intersect_point3d = slice.intersect_ray(&origin3d, &normal3d);
        let intersect_point2d = self.project(&intersect_point3d);
        let mut line_normal = self.project(&(origin3d + normal3d));
        line_normal.normalise();
        let line_d = line_normal.dot_product(&intersect_point2d);
        LineEq::new(line_normal, line_d)
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

    /// `isInFrontOf` (`planeeq.ipp:196-202`): the almostEqual buffer makes
    /// an on-plane point report "in front".
    #[test]
    fn is_in_front_of_allows_the_almost_equal_buffer() {
        let plane = PlaneEq::new(Vector3::new(0.0, 0.0, 1.0), 5.0);
        assert!(plane.is_in_front_of(&Vector3::new(1.0, 2.0, 7.0)));
        assert!(!plane.is_in_front_of(&Vector3::new(1.0, 2.0, 3.0)));
        // On-plane, and within the 0.0004 buffer either side: front.
        assert!(plane.is_in_front_of(&Vector3::new(0.0, 0.0, 5.0)));
        assert!(plane.is_in_front_of(&Vector3::new(0.0, 0.0, 5.0 + 3e-4)));
        assert!(plane.is_in_front_of(&Vector3::new(0.0, 0.0, 5.0 - 3e-4)));
        // Past the buffer the strict comparison decides.
        assert!(!plane.is_in_front_of(&Vector3::new(0.0, 0.0, 5.0 - 1e-3)));
    }

    /// `basis` picks the normal's biggest axis and builds both directions on
    /// it (`planeeq.cpp:92-115`); `param`/`project` are inverse mappings.
    #[test]
    fn basis_param_and_project_roundtrip() {
        let plane = PlaneEq::new(Vector3::new(0.0, 0.0, 1.0), 5.0);
        let (xdir, ydir) = plane.basis();
        // Biggest axis is z: xdir is +x, ydir = n × xdir = +y.
        assert_eq!(xdir, Vector3::new(1.0, 0.0, 0.0));
        assert_eq!(ydir, Vector3::new(0.0, 1.0, 0.0));
        // param lifts the 2D basis point onto the plane at height d.
        let p = plane.param(Vector2::new(3.0, -2.0));
        assert_almost_eq(p.x, 3.0);
        assert_almost_eq(p.y, -2.0);
        assert_almost_eq(p.z, 5.0);
        // project brings it back.
        let q = plane.project(&p);
        assert_almost_eq(q.x, 3.0);
        assert_almost_eq(q.y, -2.0);

        // A slanted plane: biggest axis x → xdir has its slope in z.
        let mut n = Vector3::new(2.0, 0.0, 1.0);
        n.normalise();
        let slanted = PlaneEq::new(n, 1.0);
        let (sx, sy) = slanted.basis();
        assert_almost_eq(sx.length(), 1.0);
        assert_almost_eq(sy.length(), 1.0);
        assert_almost_eq(sx.dot_product(&sy), 0.0);
        let sp = slanted.param(Vector2::new(1.5, 0.5));
        assert!(
            slanted.distance_to(&sp).abs() < 1e-4,
            "param lands on the plane"
        );
        let sq = slanted.project(&sp);
        assert_almost_eq(sq.x, 1.5);
        assert_almost_eq(sq.y, 0.5);
    }

    /// `intersect` (`planeeq.cpp:24-76`): the cut of two planes is a line in
    /// the first plane's basis; the zero normal marks a parallel slice.
    #[test]
    fn intersect_gives_the_cut_line_and_zero_for_parallel() {
        let ground = PlaneEq::new(Vector3::new(0.0, 0.0, 1.0), 2.0);
        let wall = PlaneEq::new(Vector3::new(1.0, 0.0, 0.0), 3.0);
        let line = ground.intersect(&wall);
        // The line normal comes from project(origin + normal3D) with the
        // un-scaled cross product, so it is (-1, 0) here with d -3: the same
        // line as (1, 0) with d 3 (param(4) is the same point either way).
        assert_almost_eq(line.normal().x, -1.0);
        assert_almost_eq(line.normal().y, 0.0);
        assert_almost_eq(line.d(), -3.0);
        // The line param lands on both planes.
        let p = ground.param(line.param(4.0));
        assert_almost_eq(p.x, 3.0);
        assert_almost_eq(p.z, 2.0);
        assert_almost_eq(ground.distance_to(&p), 0.0);
        assert_almost_eq(wall.distance_to(&p), 0.0);

        // Parallel slices give the zero line.
        let flat = ground.intersect(&PlaneEq::new(Vector3::new(0.0, 0.0, 1.0), 7.0));
        assert_eq!(flat.normal(), &Vector2::ZERO);
        assert_eq!(flat.d(), 0.0);
    }
}
