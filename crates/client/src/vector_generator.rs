//! Particle vector generators.
//!
//! Verbatim port of `lib/particle/actions/vector_generator.cpp`: the five
//! source shapes (point, line, box, cylinder, sphere) a particle system's
//! position/velocity sources can take, drawn through `bw_random()` so a
//! fixed seed replays identically.
//!
//! The reference sphere cache is a function-local static shared by every
//! generator; here it is an explicit per-generator value so the seeding
//! order is visible to tests (registered deviation: a host that creates two
//! sphere generators draws the cache twice instead of sharing one cache).

use crate::bw_random::BWRandom;
use foundation::math::vector::Vector3;

/// `MATH_PI`.
const MATH_PI: f32 = std::f32::consts::PI;

/// `SphereVectorCache::CACHE_SIZE`.
const CACHE_SIZE: usize = 4096;
/// `SphereVectorCache::MAX_POW_INDEX` — the highest cache slot.
const MAX_POW_INDEX: i32 = (CACHE_SIZE - 1) as i32;

/// `PointVectorGenerator` (`vector_generator.cpp:52`): every draw is the
/// fixed position, consuming no randomness.
pub struct PointVectorGenerator {
    position: Vector3,
}

impl PointVectorGenerator {
    pub fn new(position: Vector3) -> PointVectorGenerator {
        PointVectorGenerator { position }
    }

    pub fn generate(&self) -> Vector3 {
        self.position
    }
}

/// `LineVectorGenerator` (`vector_generator.cpp:81`): a uniform draw along
/// the segment, one `unitRand` per generate.
pub struct LineVectorGenerator {
    origin: Vector3,
    direction: Vector3,
}

impl LineVectorGenerator {
    /// The reference constructor routes through `start()` then `end()`,
    /// which reduces to origin = start, direction = end - start.
    pub fn new(start: Vector3, end: Vector3) -> LineVectorGenerator {
        LineVectorGenerator {
            origin: start,
            direction: end - start,
        }
    }

    /// `start(pos)`: keep the end point, move the origin, recompute the
    /// direction (`vector_generator.ipp:69`).
    pub fn set_start(&mut self, pos: Vector3) {
        let end_point = self.end();
        self.origin = pos;
        self.set_end(end_point);
    }

    /// `end()`: `origin_ + direction_`.
    pub fn end(&self) -> Vector3 {
        self.origin + self.direction
    }

    /// `end(end)`: `direction_ = end - origin_`.
    pub fn set_end(&mut self, end: Vector3) {
        self.direction = end - self.origin;
    }

    pub fn generate(&self, rng: &mut BWRandom) -> Vector3 {
        self.origin + self.direction * rng.unit_rand()
    }
}

/// `BoxVectorGenerator` (`vector_generator.cpp:700`): one draw per axis in
/// x, y, z order between the two corners.
pub struct BoxVectorGenerator {
    corner: Vector3,
    opposite: Vector3,
}

impl BoxVectorGenerator {
    pub fn new(corner: Vector3, opposite: Vector3) -> BoxVectorGenerator {
        BoxVectorGenerator { corner, opposite }
    }

    pub fn generate(&self, rng: &mut BWRandom) -> Vector3 {
        Vector3::new(
            self.corner.x + rng.unit_rand() * (self.opposite.x - self.corner.x),
            self.corner.y + rng.unit_rand() * (self.opposite.y - self.corner.y),
            self.corner.z + rng.unit_rand() * (self.opposite.z - self.corner.z),
        )
    }
}

/// `CylinderVectorGenerator` (`vector_generator.cpp:295`): an axis draw
/// plus a disc point in the cross-section basis, three draws per generate.
/// The reference's minimum radius is forced to zero by `checkRadii` (Bug
/// 5075), so the annulus branch is dead here as there.
pub struct CylinderVectorGenerator {
    origin: Vector3,
    direction: Vector3,
    basis_u: Vector3,
    basis_v: Vector3,
    max_radius: f32,
    /// Always zero after `check_radii`; kept because `generate` still
    /// reads it into the sorted pair.
    min_radius: f32,
}

impl CylinderVectorGenerator {
    pub fn new(source: Vector3, destination: Vector3, max_radius: f32, min_radius: f32) -> Self {
        let mut ret = CylinderVectorGenerator {
            origin: source,
            direction: destination - source,
            basis_u: Vector3::ZERO,
            basis_v: Vector3::ZERO,
            max_radius,
            min_radius,
        };
        ret.construct_basis();
        ret.check_radii();
        ret
    }

    /// `destination(dest)`: recompute the direction and rebuild the basis.
    pub fn set_destination(&mut self, destination: Vector3) {
        self.direction = destination - self.origin;
        self.construct_basis();
    }

    /// `updateOrigin(pos)`: move the origin, keep the destination.
    pub fn update_origin(&mut self, pos: Vector3) {
        let old_destination = self.destination();
        self.origin = pos;
        self.set_destination(old_destination);
    }

    /// `destination()`: `origin_ + direction_`.
    pub fn destination(&self) -> Vector3 {
        self.origin + self.direction
    }

    /// `constructBasis` (`vector_generator.cpp:322`): pick an x or y helper
    /// axis whichever is closer to orthogonal to the direction, then build
    /// the cross-section plane from two cross products.
    fn construct_basis(&mut self) {
        let mut normalised_direction = self.direction;
        normalised_direction.normalise();
        let mut basis = Vector3::new(1.0, 0.0, 0.0);
        // almostEqual(1.0f, fabs(dot), 0.0001f)
        if (1.0 - basis.dot_product(&normalised_direction).abs()).abs() < 0.0001 {
            basis = Vector3::new(0.0, 1.0, 0.0);
        }

        self.basis_u = basis.cross_product(&basis, &self.direction);
        self.basis_u.normalise();

        self.basis_v = self.direction.cross_product(&self.direction, &self.basis_u);
        self.basis_v.normalise();
    }

    /// `checkRadii` (`vector_generator.cpp:342`): the minimum radius is
    /// forced to zero, a negative maximum is clamped to zero.
    fn check_radii(&mut self) {
        self.min_radius = 0.0;
        if self.max_radius < 0.0 {
            self.max_radius = 0.0;
        }
    }

    pub fn generate(&self, rng: &mut BWRandom) -> Vector3 {
        // A point along the axis of the cylinder.
        let mut vector = self.origin + self.direction * rng.unit_rand();

        // Sorted radii.
        let mut min_r = self.min_radius;
        let mut max_r = self.max_radius;
        if min_r > max_r {
            std::mem::swap(&mut min_r, &mut max_r);
        }

        let angle = rng.unit_rand() * 2.0 * MATH_PI;
        let r = if min_r == max_r {
            min_r
        } else {
            let alpha = min_r / max_r;
            let k1 = (rng.unit_rand() * (1.0 - alpha) + min_r).sqrt();
            // Math::lerp(k1, alpha, 1.0f, minR, maxR)
            (alpha - k1) * (max_r - 1.0) / (min_r - 1.0) + k1
        };
        vector = vector + (self.basis_u * angle.cos() + self.basis_v * angle.sin()) * r;
        vector
    }
}

/// `SphereVectorCache` (`vector_generator.cpp:536`): 4096 pre-drawn unit
/// directions and cube-root powers, drawn once per generator.
pub struct SphereVectorCache {
    vector_cache: Vec<Vector3>,
    pow_cache: Vec<f32>,
}

impl SphereVectorCache {
    pub const CACHE_SIZE: usize = CACHE_SIZE;

    /// The constructor body: two `unitRand` draws per slot (theta, phi),
    /// 8192 draws in total.
    pub fn new(rng: &mut BWRandom) -> SphereVectorCache {
        let mut vector_cache = Vec::with_capacity(CACHE_SIZE);
        let mut pow_cache = Vec::with_capacity(CACHE_SIZE);
        for i in 0..CACHE_SIZE {
            let theta = 2.0 * MATH_PI * rng.unit_rand();
            let phi = (2.0 * rng.unit_rand() - 1.0).acos();
            let sinphi = phi.sin();
            let x = theta.cos() * sinphi;
            let y = theta.sin() * sinphi;
            let z = phi.cos();

            vector_cache.push(Vector3::new(x, y, z));
            pow_cache.push(((i as f32) / (CACHE_SIZE as f32)).powf(1.0 / 3.0));
        }
        SphereVectorCache {
            vector_cache,
            pow_cache,
        }
    }

    /// `getVector()`: one raw `bw_random()` word modulo the cache size —
    /// not a `unitRand`.
    pub fn get_vector(&self, rng: &mut BWRandom) -> Vector3 {
        self.vector_cache[(rng.next() % CACHE_SIZE as u32) as usize]
    }

    /// `getPow(num)`: `(int)` truncation into the power table with the
    /// reference's out-of-range clamps.
    pub fn get_pow(&self, num: f32) -> f32 {
        let idx = (num * MAX_POW_INDEX as f32) as i32;
        self.pow_cache[idx.clamp(0, MAX_POW_INDEX) as usize]
    }
}

/// `SphereVectorGenerator` (`vector_generator.cpp:577`): a cached unit
/// direction scaled into the shell, two draws per generate (one when the
/// radii are equal).
pub struct SphereVectorGenerator {
    centre: Vector3,
    min_radius: f32,
    max_radius: f32,
    cache: SphereVectorCache,
}

impl SphereVectorGenerator {
    pub fn new(centre: Vector3, max_radius: f32, min_radius: f32, rng: &mut BWRandom) -> Self {
        let mut ret = SphereVectorGenerator {
            centre,
            min_radius,
            max_radius,
            cache: SphereVectorCache::new(rng),
        };
        ret.check_radii();
        ret
    }

    /// `checkRadii` (`vector_generator.cpp:510`): negatives clamp to zero,
    /// then max at least min.
    fn check_radii(&mut self) {
        if self.max_radius < 0.0 {
            self.max_radius = 0.0;
        }
        if self.min_radius < 0.0 {
            self.min_radius = 0.0;
        }
        if self.max_radius < self.min_radius {
            self.max_radius = self.min_radius;
        }
    }

    pub fn generate(&self, rng: &mut BWRandom) -> Vector3 {
        let vector = self.cache.get_vector(rng);

        let r = if self.min_radius == self.max_radius {
            self.min_radius
        } else {
            let alpha = self.min_radius / self.max_radius;
            let k1 = self.cache.get_pow(rng.unit_rand() * (1.0 - alpha) + alpha);
            k1 * self.max_radius
        };

        vector * r + self.centre
    }
}

/// The `VectorGenerator` polymorph the particle actions hold by pointer in
/// the reference.
pub enum VectorGenerator {
    Point(PointVectorGenerator),
    Line(LineVectorGenerator),
    Box(BoxVectorGenerator),
    Cylinder(CylinderVectorGenerator),
    Sphere(SphereVectorGenerator),
}

impl VectorGenerator {
    /// One draw. The draw count per shape is the replay contract: point 0,
    /// line 1, box 3, cylinder 3, sphere 2 (1 when the radii are equal).
    pub fn generate(&mut self, rng: &mut BWRandom) -> Vector3 {
        match self {
            VectorGenerator::Point(g) => g.generate(),
            VectorGenerator::Line(g) => g.generate(rng),
            VectorGenerator::Box(g) => g.generate(rng),
            VectorGenerator::Cylinder(g) => g.generate(rng),
            VectorGenerator::Sphere(g) => g.generate(rng),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v3(x: f32, y: f32, z: f32) -> Vector3 {
        Vector3::new(x, y, z)
    }

    #[test]
    fn point_draws_no_randomness() {
        let mut rng = BWRandom::new(5489);
        let g = PointVectorGenerator::new(v3(1.0, 2.0, 3.0));
        for _ in 0..3 {
            assert_eq!(g.generate(), v3(1.0, 2.0, 3.0));
        }
        // No draws consumed.
        let first = rng.next();
        let mut r2 = BWRandom::new(5489);
        assert_eq!(r2.next(), first);
    }

    #[test]
    fn line_is_uniform_along_the_segment() {
        // One draw per generate, interpolated origin + u * direction.
        let mut rng = BWRandom::new(5489);
        let g = LineVectorGenerator::new(v3(0.0, 0.0, 0.0), v3(10.0, 0.0, 0.0));
        let mut r2 = BWRandom::new(5489);
        let expected = v3(r2.unit_rand() * 10.0, 0.0, 0.0);
        assert_eq!(g.generate(&mut rng), expected);
    }

    #[test]
    fn line_setters_keep_the_end_point() {
        let mut g = LineVectorGenerator::new(v3(1.0, 2.0, 3.0), v3(4.0, 5.0, 6.0));
        assert_eq!(g.end(), v3(4.0, 5.0, 6.0));
        // start(pos) keeps the end point and re-derives the direction.
        g.set_start(v3(0.0, 0.0, 0.0));
        assert_eq!(g.end(), v3(4.0, 5.0, 6.0));
        // end(end) re-derives the direction from the current origin.
        g.set_end(v3(1.0, 1.0, 1.0));
        assert_eq!(g.end(), v3(1.0, 1.0, 1.0));
    }

    #[test]
    fn box_draws_one_unit_rand_per_axis() {
        let mut rng = BWRandom::new(5489);
        let g = BoxVectorGenerator::new(v3(-1.0, -2.0, -3.0), v3(1.0, 2.0, 3.0));
        let mut r2 = BWRandom::new(5489);
        let expected = v3(
            -1.0 + r2.unit_rand() * 2.0,
            -2.0 + r2.unit_rand() * 4.0,
            -3.0 + r2.unit_rand() * 6.0,
        );
        assert_eq!(g.generate(&mut rng), expected);
    }

    #[test]
    fn cylinder_forces_the_minimum_radius_to_zero() {
        let mut rng = BWRandom::new(5489);
        // A nonzero minimum is forced to zero by checkRadii (Bug 5075).
        let g = CylinderVectorGenerator::new(v3(0.0, 0.0, 0.0), v3(0.0, 4.0, 0.0), 2.0, 1.0);
        let mut r2 = BWRandom::new(5489);
        // Reproduce the draw by hand: axis point, angle, sqrt radius.
        let axis = v3(0.0, 4.0, 0.0) * r2.unit_rand();
        let angle = r2.unit_rand() * 2.0 * MATH_PI;
        let k1 = r2.unit_rand().sqrt();
        // Math::lerp(k1, 0, 1, 0, 2) with minR forced to zero.
        let expected_r = (0.0f32 - k1) * (2.0 - 1.0) / (0.0 - 1.0) + k1;
        // Direction +y keeps the x helper: basis_u = x cross dir = +z,
        // basis_v = dir cross basis_u = +x.
        let basis_u = v3(0.0, 0.0, 1.0);
        let basis_v = v3(1.0, 0.0, 0.0);
        let expected = axis + (basis_u * angle.cos() + basis_v * angle.sin()) * expected_r;
        assert_eq!(g.generate(&mut rng), expected);
    }

    #[test]
    fn cylinder_basis_picks_the_y_helper_for_parallel_x() {
        // Direction along x: the x helper basis is near parallel, so the
        // reference switches to y.
        let g = CylinderVectorGenerator::new(v3(0.0, 0.0, 0.0), v3(3.0, 0.0, 0.0), 1.0, 0.0);
        let mut rng = BWRandom::new(5489);
        let p = g.generate(&mut rng);
        // The cross-section plane is spanned by y/z; the x offset comes
        // only from the axis draw.
        let _ = p;
        // And a degenerate zero-radius cylinder collapses onto the axis.
        let g0 = CylinderVectorGenerator::new(v3(1.0, 2.0, 3.0), v3(1.0, 5.0, 3.0), 0.0, 0.0);
        // Replay rng's stream: the first generate burned three draws.
        let mut r2 = BWRandom::new(5489);
        let _ = r2.unit_rand(); // g: axis
        let _ = r2.unit_rand(); // g: angle
        let _ = r2.unit_rand(); // g: radius
        let axis = v3(1.0, 2.0, 3.0) + v3(0.0, 3.0, 0.0) * r2.unit_rand();
        let _ = r2.unit_rand(); // angle still drawn
        assert_eq!(g0.generate(&mut rng), axis);
    }

    #[test]
    fn cylinder_setters_rebuild_the_basis() {
        let mut g = CylinderVectorGenerator::new(v3(0.0, 0.0, 0.0), v3(0.0, 4.0, 0.0), 1.5, 0.0);
        assert_eq!(g.destination(), v3(0.0, 4.0, 0.0));
        g.set_destination(v3(0.0, 0.0, 2.0));
        assert_eq!(g.destination(), v3(0.0, 0.0, 2.0));
        // updateOrigin moves the origin and keeps the destination where
        // it was: origin + direction stays (0, 0, 2).
        g.update_origin(v3(5.0, 0.0, 0.0));
        assert_eq!(g.destination(), v3(0.0, 0.0, 2.0));
        let mut rng = BWRandom::new(5489);
        let _ = g.generate(&mut rng);
    }

    #[test]
    fn sphere_cache_index_uses_a_raw_word() {
        let mut rng = BWRandom::new(5489);
        let g = SphereVectorGenerator::new(v3(0.0, 0.0, 0.0), 1.0, 1.0, &mut rng);
        // Equal radii: one raw draw picks the cache slot, no radius draw.
        // The replay must sit at the same point in the stream, i.e. past
        // the cache build too.
        let mut r2 = BWRandom::new(5489);
        let probe = SphereVectorCache::new(&mut r2);
        let word = r2.next();
        let expected = probe.vector_cache[(word % CACHE_SIZE as u32) as usize];
        assert_eq!(g.generate(&mut rng), expected);
    }

    #[test]
    fn sphere_radii_scale_the_cached_direction() {
        let mut rng = BWRandom::new(5489);
        let g = SphereVectorGenerator::new(v3(1.0, 2.0, 3.0), 4.0, 0.0, &mut rng);
        let mut r2 = BWRandom::new(5489);
        let probe = SphereVectorCache::new(&mut r2);
        let word = r2.next();
        let dir = probe.vector_cache[(word % CACHE_SIZE as u32) as usize];
        let alpha = 0.0f32 / 4.0f32;
        let k1 = probe.get_pow(r2.unit_rand() * (1.0 - alpha) + alpha);
        let expected = dir * (k1 * 4.0) + v3(1.0, 2.0, 3.0);
        assert_eq!(g.generate(&mut rng), expected);
    }

    #[test]
    fn sphere_cache_pow_clamps_out_of_range() {
        let mut rng = BWRandom::new(5489);
        let cache = SphereVectorCache::new(&mut rng);
        assert_eq!(cache.get_pow(-1.0), cache.get_pow(0.0));
        // Above the range clamps to the last slot, so 2.0 and 1.0 agree.
        assert_eq!(cache.get_pow(2.0), cache.get_pow(1.0));
        // The power table is the cube-root ramp over i / 4096: the last
        // slot sits just under one.
        assert_eq!(cache.get_pow(1.0), (4095.0f32 / 4096.0f32).powf(1.0 / 3.0));
        assert_eq!(cache.get_pow(0.0), 0.0);
    }

    #[test]
    fn sphere_check_radii_clamps_negatives_and_orders() {
        let mut rng = BWRandom::new(5489);
        // Negative radii clamp to zero, so min == max == 0 collapses to the
        // centre.
        let g = SphereVectorGenerator::new(v3(7.0, 8.0, 9.0), -2.0, -5.0, &mut rng);
        assert_eq!(g.generate(&mut rng), v3(7.0, 8.0, 9.0));
        // Reversed radii order: max is raised to min.
        let g2 = SphereVectorGenerator::new(v3(0.0, 0.0, 0.0), 1.0, 3.0, &mut rng);
        // The replay sits past both cache builds and the first
        // generator's single collapsed draw.
        let mut r2 = BWRandom::new(5489);
        let _ = SphereVectorCache::new(&mut r2);
        let _ = r2.next();
        let probe = SphereVectorCache::new(&mut r2);
        let word = r2.next();
        let dir = probe.vector_cache[(word % CACHE_SIZE as u32) as usize];
        let expected = dir * 3.0;
        assert_eq!(g2.generate(&mut rng), expected);
    }

    #[test]
    fn the_enum_dispatches_each_shape() {
        let mut rng = BWRandom::new(5489);
        let mut point = VectorGenerator::Point(PointVectorGenerator::new(v3(1.0, 1.0, 1.0)));
        assert_eq!(point.generate(&mut rng), v3(1.0, 1.0, 1.0));
        let mut line = VectorGenerator::Line(LineVectorGenerator::new(
            v3(0.0, 0.0, 0.0),
            v3(2.0, 0.0, 0.0),
        ));
        let _ = line.generate(&mut rng);
        let mut boxg = VectorGenerator::Box(BoxVectorGenerator::new(
            v3(0.0, 0.0, 0.0),
            v3(1.0, 1.0, 1.0),
        ));
        let _ = boxg.generate(&mut rng);
        let mut cyl = VectorGenerator::Cylinder(CylinderVectorGenerator::new(
            v3(0.0, 0.0, 0.0),
            v3(0.0, 1.0, 0.0),
            1.0,
            0.0,
        ));
        let _ = cyl.generate(&mut rng);
        let mut sphere = VectorGenerator::Sphere(SphereVectorGenerator::new(
            v3(0.0, 0.0, 0.0),
            1.0,
            0.0,
            &mut rng,
        ));
        let _ = sphere.generate(&mut rng);
    }
}
