//! BW-01-01-04 quaternions, following the reference scalar fallback path in
//! `lib/math/quat.cpp`.
//!
//! Component order is `{x, y, z, w}`. There is no direct Euler constructor in
//! the reference: Euler angles go through `Matrix::set_rotate_ypr` +
//! `Quaternion::from_matrix`.

use super::matrix::Matrix;
use super::vector::{Vector3, Vector4, clamp};

/// Default epsilon for the reference `almostEqual` equality.
pub const QUATERNION_ALMOST_EQUAL_EPSILON: f32 = 0.0004;

#[derive(Debug, Clone, Copy, PartialEq, Default)]
#[repr(C)]
pub struct Quaternion {
    pub x: f32,
    pub y: f32,
    pub z: f32,
    pub w: f32,
}

impl Quaternion {
    pub const IDENTITY: Self = Self {
        x: 0.0,
        y: 0.0,
        z: 0.0,
        w: 1.0,
    };

    pub const fn new(x: f32, y: f32, z: f32, w: f32) -> Self {
        Self { x, y, z, w }
    }

    pub fn dot_product(&self, other: &Self) -> f32 {
        self.x * other.x + self.y * other.y + self.z * other.z + self.w * other.w
    }

    pub fn length(&self) -> f32 {
        self.length_squared().sqrt()
    }

    pub fn length_squared(&self) -> f32 {
        self.dot_product(self)
    }

    /// Reference `normalise()` (`quat.cpp:173-181`): scales by the reciprocal
    /// with *no* zero-length protection — a zero quaternion yields
    /// infinities/NaN, matching the Linux build.
    pub fn normalise(&mut self) {
        let invd = 1.0 / self.length();
        self.x *= invd;
        self.y *= invd;
        self.z *= invd;
        self.w *= invd;
    }

    /// Full inverse: conjugate divided by length-squared (`quat.cpp:186-194`).
    pub fn invert(&mut self) {
        let d = self.length_squared();
        self.x = -self.x / d;
        self.y = -self.y / d;
        self.z = -self.z / d;
        self.w /= d;
    }

    /// Conjugate (no division), commonly used for unit quaternions.
    pub fn conjugate(&mut self) {
        self.x = -self.x;
        self.y = -self.y;
        self.z = -self.z;
    }

    /// Flip all components when `w` is negative so `w >= 0`
    /// (`quat.ipp:180-189`).
    pub fn minimise(&mut self) {
        if self.w < 0.0 {
            self.x = -self.x;
            self.y = -self.y;
            self.z = -self.z;
            self.w = -self.w;
        }
    }

    /// Hamilton product with the reference's operand order: the result
    /// applies `q1` first, then `q2` (`quat.cpp:151-166`).
    pub fn multiply(q1: &Self, q2: &Self) -> Self {
        let q1v = Vector3::new(q1.x, q1.y, q1.z);
        let q2v = Vector3::new(q2.x, q2.y, q2.z);

        // v = q2v × q1v (note the swapped operands).
        let v = Vector3::ZERO.cross_product(&q2v, &q1v);

        Self {
            x: (q2.w * q1v.x) + (q1.w * q2v.x) + v.x,
            y: (q2.w * q1v.y) + (q1.w * q2v.y) + v.y,
            z: (q2.w * q1v.z) + (q1.w * q2v.z) + v.z,
            w: (q2.w * q1.w) - q2v.dot_product(&q1v),
        }
    }

    /// `this = q ⊗ this`-shaped helper: `multiply(q, this)`.
    pub fn pre_multiply(&mut self, q: &Self) {
        *self = Self::multiply(q, self);
    }

    /// `postMultiply(q)`: `multiply(this, q)`.
    pub fn post_multiply(&mut self, q: &Self) {
        *self = Self::multiply(self, q);
    }

    /// Half-angle axis rotation (`quat.cpp:80-96`). A zero-length axis leaves
    /// the vector part at zero (reference `normalise` is a no-op there).
    pub fn from_angle_axis(angle: f32, axis: &Vector3) -> Self {
        let theta = angle * 0.5;
        let sin_theta = theta.sin();

        let mut v = *axis;
        v.normalise();
        v.scale(sin_theta);

        Self {
            x: v.x,
            y: v.y,
            z: v.z,
            w: theta.cos(),
        }
    }

    /// Shepperd's method, pinned to the reference branches
    /// (`quat.cpp:104-144`), including the `isZero` guard on the diagonal
    /// branch.
    pub fn from_matrix(m: &Matrix) -> Self {
        let mut q = Self::default();
        let tr = m.m[0][0] + m.m[1][1] + m.m[2][2];

        if tr > 0.0 {
            let mut s = (tr + 1.0).sqrt();
            q.w = s * 0.5;
            s = 0.5 / s;
            q.x = (m.m[1][2] - m.m[2][1]) * s;
            q.y = (m.m[2][0] - m.m[0][2]) * s;
            q.z = (m.m[0][1] - m.m[1][0]) * s;
        } else {
            let mut i = 0usize;
            if m.m[1][1] > m.m[0][0] {
                i = 1;
            }
            if m.m[2][2] > m.m[i][i] {
                i = 2;
            }

            let j = (i + 1) % 3;
            let k = (j + 1) % 3;

            let mut s = ((m.m[i][i] - (m.m[j][j] + m.m[k][k])) + 1.0).sqrt();
            q.set_component(i, s * 0.5);

            // Reference `isZero` is exact: `fabsf(s) <= 0`.
            if s != 0.0 {
                s = 0.5 / s;
            }

            q.w = (m.m[j][k] - m.m[k][j]) * s;
            q.set_component(j, (m.m[i][j] + m.m[j][i]) * s);
            q.set_component(k, (m.m[i][k] + m.m[k][i]) * s);
        }
        q
    }

    fn set_component(&mut self, i: usize, value: f32) {
        match i {
            0 => self.x = value,
            1 => self.y = value,
            _ => self.z = value,
        }
    }

    fn get_component(&self, i: usize) -> f32 {
        match i {
            0 => self.x,
            1 => self.y,
            _ => self.z,
        }
    }

    /// Reference `slerp` (`quat.cpp:32-70`): short-arc (flips the end
    /// quaternion's contribution when the dot is negative), linear
    /// interpolation when `1 − cos ≤ 0.001`, spherical otherwise, and the
    /// result is *not* re-normalised.
    pub fn slerp(q_start: &Self, q_end: &Self, t: f32) -> Self {
        let mut cos_theta = q_start.dot_product(q_end);

        let mut invert = false;
        if cos_theta < 0.0 {
            cos_theta = -cos_theta;
            invert = true;
        }

        let mut t2 = 1.0 - t;

        if 1.0 - cos_theta > 0.001 {
            let theta = clamp(-1.0, cos_theta, 1.0).acos();
            t2 = (theta * t2).sin() / theta.sin();
            let t = (theta * t).sin() / theta.sin();
            let mut tt = t;
            if invert {
                tt = -tt;
            }
            return Self::blend(q_start, q_end, t2, tt);
        }

        let mut tt = t;
        if invert {
            tt = -tt;
        }
        Self::blend(q_start, q_end, t2, tt)
    }

    fn blend(q_start: &Self, q_end: &Self, t2: f32, t: f32) -> Self {
        let qs = Vector4::new(q_start.x, q_start.y, q_start.z, q_start.w);
        let qe = Vector4::new(q_end.x, q_end.y, q_end.z, q_end.w);
        let r = qs * t2 + qe * t;
        Self::new(r.x, r.y, r.z, r.w)
    }

    /// Reference equality semantics: componentwise
    /// `fabs(diff) < 0.0004`. Rust's derived `==` stays exact; call this where
    /// the reference used `operator==`.
    pub fn almost_equal(&self, other: &Self) -> bool {
        (self.x - other.x).abs() < QUATERNION_ALMOST_EQUAL_EPSILON
            && (self.y - other.y).abs() < QUATERNION_ALMOST_EQUAL_EPSILON
            && (self.z - other.z).abs() < QUATERNION_ALMOST_EQUAL_EPSILON
            && (self.w - other.w).abs() < QUATERNION_ALMOST_EQUAL_EPSILON
    }

    /// Index-based access mirroring `(*this)[i]` (x, y, z; index 3 is w).
    pub fn get(&self, i: usize) -> f32 {
        if i == 3 {
            self.w
        } else {
            self.get_component(i)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn multiply_order_applies_q1_then_q2() {
        // q1 = 90° yaw; q2 = 90° pitch. Applying q1 then q2 gives a
        // distinguishable orientation from the reverse order.
        let q1 = Quaternion::from_angle_axis(std::f32::consts::FRAC_PI_2, &Vector3::J);
        let q2 = Quaternion::from_angle_axis(std::f32::consts::FRAC_PI_2, &Vector3::I);

        let q12 = Quaternion::multiply(&q1, &q2);
        let q21 = Quaternion::multiply(&q2, &q1);
        assert_ne!(q12, q21, "Hamilton product is order-sensitive");

        // Vector transformed by q12 == q2 applied to (q1 applied to v).
        let v = Vector3::K;
        let mut m1 = Matrix::zero();
        m1.set_rotate_quat(&q1);
        let mut m2 = Matrix::zero();
        m2.set_rotate_quat(&q2);
        let mut m12 = Matrix::zero();
        m12.set_rotate_quat(&q12);
        let step1 = m1.apply_vector(&v);
        let step2 = m2.apply_vector(&step1);
        let direct = m12.apply_vector(&v);
        assert!(
            super::super::vector::almost_zero(step2.x - direct.x, 1e-5)
                && super::super::vector::almost_zero(step2.y - direct.y, 1e-5)
                && super::super::vector::almost_zero(step2.z - direct.z, 1e-5),
            "multiply(q1,q2) applies q1 first"
        );
    }

    #[test]
    fn from_angle_axis_half_angle() {
        let q = Quaternion::from_angle_axis(std::f32::consts::PI, &Vector3::J);
        let theta = std::f32::consts::PI * 0.5;
        assert!((q.y - theta.sin()).abs() < 1e-6);
        assert!(q.w.abs() < 1e-6, "w = cos(θ/2) is 0 at 180°");
        assert!((q.length() - 1.0).abs() < 1e-6);

        // Zero-length axis: normalise leaves it at zero, no NaN.
        let q0 = Quaternion::from_angle_axis(1.0, &Vector3::ZERO);
        assert_eq!(q0.x, 0.0);
        assert_eq!(q0.y, 0.0);
        assert_eq!(q0.z, 0.0);
        assert!((q0.w - 0.5f32.cos()).abs() < 1e-6, "w = cos(angle/2)");
    }

    #[test]
    fn from_matrix_and_set_rotate_quat_roundtrip() {
        let yaw = 1.2f32;
        let pitch = -0.4f32;
        let roll = 0.8f32;
        let mut m = Matrix::zero();
        m.set_rotate_ypr(yaw, pitch, roll);

        let q = Quaternion::from_matrix(&m);
        let mut m2 = Matrix::zero();
        m2.set_rotate_quat(&q);
        assert!(
            m.almost(&m2, 1e-5),
            "setRotate ↔ fromMatrix roundtrip\n{:?}\n{:?}",
            m.m,
            m2.m
        );
    }

    #[test]
    fn from_matrix_diagonal_branches() {
        // 180° about Y: trace = -1 -1 +1 = -1 ≤ 0, largest diagonal is [1][1].
        let mut m = Matrix::zero();
        m.set_rotate_y(std::f32::consts::PI);
        let q = Quaternion::from_matrix(&m);
        assert!((q.y - 1.0).abs() < 1e-5, "y component dominates: {:?}", q);
        assert!(q.w.abs() < 1e-5);

        // 180° about Z: trace ≤ 0, largest diagonal [2][2].
        let mut mz = Matrix::zero();
        mz.set_rotate_z(std::f32::consts::PI);
        let qz = Quaternion::from_matrix(&mz);
        assert!((qz.z - 1.0).abs() < 1e-5, "z component dominates: {:?}", qz);
    }

    #[test]
    fn slerp_endpoints_and_short_arc() {
        let qa = Quaternion::IDENTITY;
        let qb = Quaternion::from_angle_axis(0.4, &Vector3::K);

        assert!(Quaternion::slerp(&qa, &qb, 0.0).almost_equal(&qa));
        assert!(Quaternion::slerp(&qa, &qb, 1.0).almost_equal(&qb));

        // Half-way interpolation stays near the unit sphere without being
        // exactly normalised (reference does not re-normalise).
        let mid = Quaternion::slerp(&qa, &qb, 0.5);
        assert!((mid.length() - 1.0).abs() < 1e-6);
    }

    #[test]
    fn slerp_takes_the_short_arc() {
        // End quaternion negated: same rotation, opposite sign. The dot is
        // negative, so slerp must flip and still produce the same path.
        let qa = Quaternion::IDENTITY;
        let qb = Quaternion::from_angle_axis(0.6, &Vector3::J);
        let qbneg = Quaternion::new(-qb.x, -qb.y, -qb.z, -qb.w);

        let a = Quaternion::slerp(&qa, &qb, 0.5);
        let b = Quaternion::slerp(&qa, &qbneg, 0.5);
        assert!(
            a.almost_equal(&b) || a.almost_equal(&Quaternion::new(-b.x, -b.y, -b.z, -b.w)),
            "short-arc: {:?} vs {:?}",
            a,
            b
        );
    }

    #[test]
    fn slerp_close_inputs_use_linear_blend() {
        // Two quaternions within the 0.001 threshold of each other.
        let qa = Quaternion::IDENTITY;
        let qb = Quaternion::from_angle_axis(1e-4, &Vector3::J);
        let mid = Quaternion::slerp(&qa, &qb, 0.25);
        let expected_t2 = 1.0 - 0.25;
        let expected = Quaternion::new(
            qa.x * expected_t2 + qb.x * 0.25,
            qa.y * expected_t2 + qb.y * 0.25,
            qa.z * expected_t2 + qb.z * 0.25,
            qa.w * expected_t2 + qb.w * 0.25,
        );
        assert_eq!(mid, expected, "below threshold the blend is linear");
    }

    #[test]
    fn normalise_has_no_zero_guard() {
        // Reference Linux behaviour: dividing by zero produces inf/NaN, not a
        // graceful no-op like Vector3::normalise.
        let mut q = Quaternion::default();
        q.normalise();
        assert!(q.w.is_nan() || q.w.is_infinite(), "w = {:?}", q.w);

        let mut u = Quaternion::new(1.0, 1.0, 1.0, 1.0);
        u.normalise();
        assert!((u.length() - 1.0).abs() < 1e-6);
    }

    #[test]
    fn invert_is_conjugate_over_length_squared() {
        let q = Quaternion::from_angle_axis(0.7, &Vector3::new(1.0, 2.0, 3.0));
        let mut inv = q;
        inv.invert();

        let product = Quaternion::multiply(&q, &inv);
        assert!(product.almost_equal(&Quaternion::IDENTITY));

        // For unit quaternions the inverse equals the conjugate.
        let mut conj = q;
        conj.conjugate();
        assert!(inv.almost_equal(&conj));
    }

    #[test]
    fn minimise_makes_w_nonnegative() {
        let mut q = Quaternion::from_angle_axis(0.5, &Vector3::J);
        q.w = -q.w;
        q.minimise();
        assert!(q.w >= 0.0);
    }

    #[test]
    fn minimise_leaves_positive_w_untouched() {
        let mut q = Quaternion::new(0.1, 0.2, 0.3, 1.0);
        q.minimise();
        assert_eq!(q, Quaternion::new(0.1, 0.2, 0.3, 1.0), "w >= 0: no flip");
    }

    #[test]
    fn almost_equal_threshold_is_4e_minus_4() {
        let a = Quaternion::new(0.0, 0.0, 0.0, 1.0);
        let b = Quaternion::new(3.9e-4, 0.0, 0.0, 1.0);
        let c = Quaternion::new(4.1e-4, 0.0, 0.0, 1.0);
        assert!(a.almost_equal(&b));
        assert!(!a.almost_equal(&c));
    }

    #[test]
    fn pre_and_post_multiply_match_reference_operand_order() {
        let q_yaw = Quaternion::from_angle_axis(std::f32::consts::FRAC_PI_2, &Vector3::J);
        let q_pitch = Quaternion::from_angle_axis(std::f32::consts::FRAC_PI_2, &Vector3::I);

        // preMultiply(q) == multiply(q, this): q applied FIRST.
        let mut pre = q_yaw;
        pre.pre_multiply(&q_pitch);
        assert_eq!(pre, Quaternion::multiply(&q_pitch, &q_yaw));

        // postMultiply(q) == multiply(this, q): q applied SECOND.
        let mut post = q_yaw;
        post.post_multiply(&q_pitch);
        assert_eq!(post, Quaternion::multiply(&q_yaw, &q_pitch));
        assert_ne!(pre, post, "the two orders are distinct rotations");
    }

    #[test]
    fn get_covers_all_component_indices() {
        let q = Quaternion::new(1.0, 2.0, 3.0, 4.0);
        assert_eq!(q.get(0), 1.0);
        assert_eq!(q.get(1), 2.0);
        assert_eq!(q.get(2), 3.0);
        assert_eq!(q.get(3), 4.0, "index 3 is w");
    }

    #[test]
    fn slerp_linear_branch_honours_the_short_arc_flip() {
        // Close quaternions (within the 0.001 threshold) AND a negated end:
        // the linear blend path must still flip the sign.
        let qa = Quaternion::IDENTITY;
        let qb = Quaternion::from_angle_axis(1e-4, &Vector3::J);
        let qbneg = Quaternion::new(-qb.x, -qb.y, -qb.z, -qb.w);

        let direct = Quaternion::slerp(&qa, &qb, 0.5);
        let flipped = Quaternion::slerp(&qa, &qbneg, 0.5);
        // blend(qa, qbneg, 0.5, -0.5) computes (-qb.x) * (-0.5), which is
        // bit-exact with qb.x * 0.5, so the flip lands on the same value.
        assert_eq!(direct, flipped, "{direct:?} vs {flipped:?}");
    }
}
