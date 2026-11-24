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

    /// `isInFrontOf` (`planeeq.ipp:197-202`): a point within the
    /// reference's `almostEqual` epsilon of the plane counts as in front.
    pub fn is_in_front_of(&self, point: &Vector3) -> bool {
        let dot = self.normal.dot_product(point);
        if (dot - self.d).abs() < 0.0004 {
            return true;
        }
        dot > self.d
    }

    /// `isInFrontOfExact` (`planeeq.ipp:211-214`): no epsilon.
    pub fn is_in_front_of_exact(&self, point: &Vector3) -> bool {
        self.normal.dot_product(point) > self.d
    }

    /// `basis` (`planeeq.cpp:92-115`): the parameterised x/y directions of
    /// the plane. The x direction zeroes the smaller of the two components
    /// next to the normal's largest, solves the largest for the plane
    /// equation, and y is the cross product; both are normalised.
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

        // find a suitable y basis vector (cross product of existing ones)
        let mut ydir = Vector3::ZERO.cross_product(&self.normal, &xdir);

        xdir.normalise();
        ydir.normalise();
        (xdir, ydir)
    }

    /// `param` (`planeeq.cpp:122-133`): the 3D point at the given
    /// parameter in the plane's basis; the plane's origin sits at
    /// `normal * d / |normal|²`.
    pub fn param(&self, param: &super::vector::Vector2) -> Vector3 {
        let (xdir, ydir) = self.basis();
        let d_over_norm_len = self.d / self.normal.length_squared();
        self.normal * d_over_norm_len + xdir * param.x + ydir * param.y
    }

    /// `project` (`planeeq.cpp:139-152`): the parameter of the given point
    /// in the plane's basis.
    pub fn project(&self, point: &Vector3) -> super::vector::Vector2 {
        let (xdir, ydir) = self.basis();
        let offset = *point - self.normal * (self.d / self.normal.length_squared());
        super::vector::Vector2::new(xdir.dot_product(&offset), ydir.dot_product(&offset))
    }

    /// `intersect` (`planeeq.cpp:24-76`): the line where the `slice`
    /// plane cuts this plane, expressed in this plane's parameterised
    /// basis. Nearly-parallel planes give the zero line (the caller
    /// filters it by comparing the normal against zero). The reference's
    /// reconstitution sanity check only prints, so it is not ported.
    pub fn intersect(&self, slice: &PlaneEq) -> super::lineeq::LineEq {
        // find the direction of our line in 3D
        let dir3d = Vector3::ZERO.cross_product(&self.normal, &slice.normal);

        if super::vector::almost_zero(dir3d.length_squared(), 0.0001) {
            return super::lineeq::LineEq::new(super::vector::Vector2::new(0.0, 0.0), 0.0);
        }

        // find the normal to the line (on the first plane) in 3D
        let normal3d = Vector3::ZERO.cross_product(&self.normal, &dir3d);

        // find the origin of the basis plane's coordinate system in 3D
        let origin3d = self.param(&super::vector::Vector2::new(0.0, 0.0));

        // find the intersection in the direction of normal3D through
        // origin3D with the slice plane
        let intersect_point3d = slice.intersect_ray(&origin3d, &normal3d);

        // now turn the intersection point and dir3D into the basis of the
        // basis plane
        let intersect_point2d = self.project(&intersect_point3d);

        // set the normal from the normal in 3D
        let mut line_normal = self.project(&(origin3d + normal3d));
        line_normal.normalise();

        // calc d to work for the point that we know is on the line
        let line_d = line_normal.dot_product(&intersect_point2d);

        super::lineeq::LineEq::new(line_normal, line_d)
    }
}

#[cfg(test)]
mod tests {
    use super::super::vector::Vector2;
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

    /// `basis` (`planeeq.cpp:92-115`) + `param`/`project`
    /// (`planeeq.cpp:122-152`): the parameterisation round-trips and the
    /// basis spans the plane.
    #[test]
    fn basis_param_and_project_round_trip() {
        let mut plane = PlaneEq::default();
        plane.init(
            &Vector3::new(1.0, 2.0, 3.0),
            &Vector3::new(3.0, 2.0, 1.0),
            &Vector3::new(2.0, 0.0, 1.0),
        );
        let (xdir, ydir) = plane.basis();
        // both basis vectors are unit and lie in the plane
        assert_almost_eq(xdir.length(), 1.0);
        assert_almost_eq(ydir.length(), 1.0);
        assert_almost_eq(plane.normal().dot_product(&xdir), 0.0);
        assert_almost_eq(plane.normal().dot_product(&ydir), 0.0);

        // a point on the plane projects to (x, y) and param maps it back
        let on_plane = Vector3::new(3.0, 2.0, 1.0);
        let projected = plane.project(&on_plane);
        let rebuilt = plane.param(&projected);
        assert_almost_eq(rebuilt.x, on_plane.x);
        assert_almost_eq(rebuilt.y, on_plane.y);
        assert_almost_eq(rebuilt.z, on_plane.z);

        // param(0,0) is the plane origin: normal * d / |normal|²
        let origin = plane.param(&Vector2::new(0.0, 0.0));
        assert_almost_eq(plane.distance_to(&origin), 0.0);
        assert_almost_eq(origin.dot_product(plane.normal()), plane.d());

        // projected components follow the basis dot products
        let offset = on_plane - origin;
        assert_almost_eq(projected.x, xdir.dot_product(&offset));
        assert_almost_eq(projected.y, ydir.dot_product(&offset));
    }

    /// `intersect` (`planeeq.cpp:24-76`): the slice line lies in both
    /// planes; nearly-parallel planes give the zero line.
    #[test]
    fn intersect_cuts_a_line_both_planes_contain() {
        let mut a = PlaneEq::default();
        a.init(
            &Vector3::new(0.0, 0.0, 0.0),
            &Vector3::new(1.0, 0.0, 0.0),
            &Vector3::new(0.0, 1.0, 0.0),
        ); // z = 0
        let mut b = PlaneEq::default();
        b.init(
            &Vector3::new(0.0, 0.0, 0.0),
            &Vector3::new(1.0, 0.0, 0.0),
            &Vector3::new(0.0, 0.0, 1.0),
        ); // y = 0
        let line = a.intersect(&b);
        // the line's normal is unit length
        assert!((line.normal().length() - 1.0).abs() < 1e-5);
        // a point on the line satisfies both planes
        let on_line2 = line.param(4.0);
        let p3 = a.param(&on_line2);
        assert_almost_eq(a.distance_to(&p3), 0.0);
        assert_almost_eq(b.distance_to(&p3), 0.0);

        // a plane cut by itself is nearly parallel: the zero line
        let self_line = a.intersect(&a);
        assert_eq!(self_line.normal(), Vector2::ZERO);
    }

    /// `isInFrontOfExact` (`planeeq.ipp:211-214`): no epsilon arm.
    #[test]
    fn is_in_front_of_exact_has_no_epsilon() {
        let plane = PlaneEq::new(Vector3::new(0.0, 0.0, 1.0), 5.0);
        // just behind the plane but within the almostEqual epsilon: in
        // front of by the fuzzy check, behind by the exact one
        let near_behind = Vector3::new(0.0, 0.0, 4.9999);
        assert!(plane.is_in_front_of(&near_behind));
        assert!(!plane.is_in_front_of_exact(&near_behind));
        // clearly behind / clearly in front agree between the two
        assert!(!plane.is_in_front_of_exact(&Vector3::new(0.0, 0.0, 4.0)));
        assert!(plane.is_in_front_of_exact(&Vector3::new(0.0, 0.0, 6.0)));
    }
}
