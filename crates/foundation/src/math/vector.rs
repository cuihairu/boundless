//! BW-01-01-04 vectors, following the reference scalar fallback path in
//! `lib/math/` (`xp_math.hpp` non-`EXT_MATH` branch, which is what Linux
//! servers build).
//!
//! Float operation order is pinned: the reference accumulates in strict
//! statement order with plain `float` (f32) arithmetic, so Rust's f32 ops
//! reproduce it bit for bit on the same IEEE-754 semantics.

#[derive(Debug, Clone, Copy, PartialEq, Default)]
#[repr(C)]
pub struct Vector2 {
    pub x: f32,
    pub y: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
#[repr(C)]
pub struct Vector3 {
    pub x: f32,
    pub y: f32,
    pub z: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
#[repr(C)]
pub struct Vector4 {
    pub x: f32,
    pub y: f32,
    pub z: f32,
    pub w: f32,
}

/// Components are laid out in x/y/z order and stream as three little-endian
/// f32s, matching `vector3.cpp` `operator<<` (three float writes).
pub const VECTOR3_STREAM_SIZE: usize = 12;

pub const V3_ZERO: Vector3 = Vector3::ZERO;

impl Vector2 {
    pub const ZERO: Self = Self { x: 0.0, y: 0.0 };

    pub const fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }

    /// `x*v.x - y*v.x`-shaped scalar cross product (`vector2.ipp`).
    pub fn cross_product(&self, other: &Self) -> f32 {
        self.x * other.y - self.y * other.x
    }

    pub fn dot_product(&self, other: &Self) -> f32 {
        self.x * other.x + self.y * other.y
    }

    pub fn length_squared(&self) -> f32 {
        self.dot_product(self)
    }

    /// `sqrtf(lengthSquared())`, returning 0 for the zero vector
    /// (`vector2.ipp` `length()`).
    pub fn length(&self) -> f32 {
        let l = self.length_squared();
        if l <= 0.0 { 0.0 } else { l.sqrt() }
    }

    /// Leaves the vector untouched when the length is almost zero
    /// (`almostZero(f, 1e-8)`); otherwise multiplies by the reciprocal.
    pub fn normalise(&mut self) -> f32 {
        let length = self.length();
        if !(-1e-8..1e-8).contains(&length) {
            let rcp = 1.0 / length;
            self.x *= rcp;
            self.y *= rcp;
        }
        length
    }

    pub fn scale(&mut self, s: f32) {
        self.x *= s;
        self.y *= s;
    }
}

impl Vector3 {
    pub const ZERO: Self = Self {
        x: 0.0,
        y: 0.0,
        z: 0.0,
    };
    /// Unit axis vectors (`vector3.cpp`).
    pub const I: Self = Self {
        x: 1.0,
        y: 0.0,
        z: 0.0,
    };
    pub const J: Self = Self {
        x: 0.0,
        y: 1.0,
        z: 0.0,
    };
    pub const K: Self = Self {
        x: 0.0,
        y: 0.0,
        z: 1.0,
    };

    pub const fn new(x: f32, y: f32, z: f32) -> Self {
        Self { x, y, z }
    }

    /// Component accessor matching `v[0..2]` in the reference.
    pub fn get(&self, i: usize) -> f32 {
        match i {
            0 => self.x,
            1 => self.y,
            _ => self.z,
        }
    }

    pub fn set(&mut self, i: usize, value: f32) {
        match i {
            0 => self.x = value,
            1 => self.y = value,
            _ => self.z = value,
        }
    }

    /// `(x*vx + y*vy) + z*vz` — the exact accumulation order of
    /// `vector3.ipp:108-116`.
    pub fn dot_product(&self, other: &Self) -> f32 {
        self.x * other.x + self.y * other.y + self.z * other.z
    }

    /// Reference cross product with the exact component formulas of
    /// `vector3.ipp:122-133`.
    pub fn cross_product(&self, v1: &Self, v2: &Self) -> Self {
        Self {
            x: (v1.y * v2.z) - (v1.z * v2.y),
            y: (v1.z * v2.x) - (v1.x * v2.z),
            z: (v1.x * v2.y) - (v1.y * v2.x),
        }
    }

    pub fn length_squared(&self) -> f32 {
        self.dot_product(self)
    }

    /// `sqrtf((x*x) + (y*y) + (z*z))` in f32.
    pub fn length(&self) -> f32 {
        ((self.x * self.x) + (self.y * self.y) + (self.z * self.z)).sqrt()
    }

    /// `distance()` is the one place the reference widens to double
    /// (`vector3.ipp:250-254`): float inputs are promoted, the square root is
    /// taken in f64, and the result is truncated back to f32.
    pub fn distance(&self, other: &Self) -> f32 {
        let dx = (self.x - other.x) as f64;
        let dy = (self.y - other.y) as f64;
        let dz = (self.z - other.z) as f64;
        ((dx * dx) + (dy * dy) + (dz * dz)).sqrt() as f32
    }

    pub fn distance_squared(&self, other: &Self) -> f32 {
        let d = *self - *other;
        d.length_squared()
    }

    /// Reference `normalise()`: nothing happens within `almostZero(1e-8)`,
    /// otherwise scale by `1/length`. Returns the pre-normalise length.
    pub fn normalise(&mut self) -> f32 {
        let length = self.length();
        if !(-1e-8..1e-8).contains(&length) {
            let rcp = 1.0 / length;
            self.x *= rcp;
            self.y *= rcp;
            self.z *= rcp;
        }
        length
    }

    pub fn scale(&mut self, s: f32) {
        self.x *= s;
        self.y *= s;
        self.z *= s;
    }

    /// `(a*(1-t)) + (b*t)` (`vector3.ipp:146-150`).
    pub fn lerp(a: &Self, b: &Self, t: f32) -> Self {
        let one_minus = 1.0 - t;
        Self {
            x: (a.x * one_minus) + (b.x * t),
            y: (a.y * one_minus) + (b.y * t),
            z: (a.z * one_minus) + (b.z * t),
        }
    }

    /// `acosf(clamp(-1, dot, 1))` on unit vectors (`vector3.ipp:307-314`).
    pub fn get_unit_vector_angle(&self, other: &Self) -> f32 {
        clamp(-1.0, self.dot_product(other), 1.0).acos()
    }

    /// `yaw = atan2f(x, z)` for a direction vector (`vector3.ipp`).
    pub fn yaw(&self) -> f32 {
        self.x.atan2(self.z)
    }

    /// `pitch = -atan2f(y, sqrtf(x*x + z*z))` (`vector3.ipp:538-551`).
    pub fn pitch(&self) -> f32 {
        -(self.y.atan2((self.x * self.x + self.z * self.z).sqrt()))
    }

    /// `setPitchYaw`: intermediate math runs in f64 and each component is
    /// truncated back to f32 (`vector3.ipp:520-531`). Note the negated pitch
    /// sine.
    pub fn set_pitch_yaw(&mut self, pitch: f32, yaw: f32) {
        let cos_pitch = (pitch as f64).cos();
        let sin_pitch = (-(pitch as f64)).sin();
        let sin_yaw = (yaw as f64).sin();
        let cos_yaw = (yaw as f64).cos();
        self.x = (cos_pitch * sin_yaw) as f32;
        self.y = sin_pitch as f32;
        self.z = (cos_pitch * cos_yaw) as f32;
    }

    /// Stream as three little-endian f32s, x then y then z.
    pub fn to_stream_bytes(&self, out: &mut crate::stream::BinaryStream) {
        out.write_f32(self.x).write_f32(self.y).write_f32(self.z);
    }

    pub fn from_stream_bytes(
        input: &mut crate::stream::BinaryStream,
    ) -> Result<Self, crate::stream::StreamError> {
        let x = input.read_f32()?;
        let y = input.read_f32()?;
        let z = input.read_f32()?;
        Ok(Self { x, y, z })
    }
}

impl std::ops::Add for Vector2 {
    type Output = Self;
    fn add(self, rhs: Self) -> Self {
        Self::new(self.x + rhs.x, self.y + rhs.y)
    }
}

impl std::ops::Sub for Vector2 {
    type Output = Self;
    fn sub(self, rhs: Self) -> Self {
        Self::new(self.x - rhs.x, self.y - rhs.y)
    }
}

impl std::ops::Mul<f32> for Vector2 {
    type Output = Self;
    fn mul(self, s: f32) -> Self {
        Self::new(self.x * s, self.y * s)
    }
}

impl std::ops::Div<f32> for Vector2 {
    type Output = Self;
    /// Reference divides via a reciprocal multiply
    /// (`vector2.ipp:221-226`).
    fn div(self, s: f32) -> Self {
        self * (1.0f32 / s)
    }
}

impl std::ops::Add for Vector3 {
    type Output = Self;
    fn add(self, rhs: Self) -> Self {
        Self::new(self.x + rhs.x, self.y + rhs.y, self.z + rhs.z)
    }
}

impl std::ops::Sub for Vector3 {
    type Output = Self;
    fn sub(self, rhs: Self) -> Self {
        Self::new(self.x - rhs.x, self.y - rhs.y, self.z - rhs.z)
    }
}

impl std::ops::Mul<f32> for Vector3 {
    type Output = Self;
    fn mul(self, s: f32) -> Self {
        Self::new(self.x * s, self.y * s, self.z * s)
    }
}

impl std::ops::Div<f32> for Vector3 {
    type Output = Self;
    /// Reference divides by computing `1.f/s` first and multiplying
    /// (`vector3.ipp:356-363`).
    fn div(self, s: f32) -> Self {
        self * (1.0f32 / s)
    }
}

impl std::ops::Neg for Vector3 {
    type Output = Self;
    fn neg(self) -> Self {
        Self::new(-self.x, -self.y, -self.z)
    }
}

impl Vector4 {
    pub const ZERO: Self = Self {
        x: 0.0,
        y: 0.0,
        z: 0.0,
        w: 0.0,
    };

    pub const fn new(x: f32, y: f32, z: f32, w: f32) -> Self {
        Self { x, y, z, w }
    }

    /// Four-term accumulation in reference order (`vector4.ipp:324-331`).
    pub fn dot_product(&self, other: &Self) -> f32 {
        other.x * self.x + other.y * self.y + other.z * self.z + other.w * self.w
    }

    pub fn length_squared(&self) -> f32 {
        self.dot_product(self)
    }

    pub fn length(&self) -> f32 {
        self.length_squared().sqrt()
    }

    pub fn normalise(&mut self) -> f32 {
        let length = self.length();
        if !(-1e-8..1e-8).contains(&length) {
            let rcp = 1.0 / length;
            self.x *= rcp;
            self.y *= rcp;
            self.z *= rcp;
            self.w *= rcp;
        }
        length
    }
}

impl std::ops::Add for Vector4 {
    type Output = Self;
    fn add(self, rhs: Self) -> Self {
        Self::new(
            self.x + rhs.x,
            self.y + rhs.y,
            self.z + rhs.z,
            self.w + rhs.w,
        )
    }
}

impl std::ops::Mul<f32> for Vector4 {
    type Output = Self;
    fn mul(self, s: f32) -> Self {
        Self::new(self.x * s, self.y * s, self.z * s, self.w * s)
    }
}

/// Reference `Math::clamp` (`stdmf.hpp`): NaN handling follows the branch
/// order, returning `min` for NaN input.
pub fn clamp(min: f32, value: f32, max: f32) -> f32 {
    if value < min {
        min
    } else if value > max {
        max
    } else {
        value
    }
}

/// `almostZero(f, eps)`: `-eps < f && f < eps` (`stdmf.hpp:616-618`).
pub fn almost_zero(value: f32, epsilon: f32) -> bool {
    -epsilon < value && value < epsilon
}

/// `almostEqual(f1, f2)`: `fabsf(f1 - f2) < 0.0004` (`stdmf.hpp:606-609`),
/// the buffer `PlaneEq::isInFrontOf` allows around its plane.
pub fn almost_equal(f1: f32, f2: f32) -> bool {
    (f1 - f2).abs() < 0.0004
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_is_component_order() {
        let v = Vector3::new(1.0, 2.0, 3.0);
        assert_eq!(v.get(0), 1.0);
        assert_eq!(v.get(1), 2.0);
        assert_eq!(v.get(2), 3.0);
        let w = Vector4::new(1.0, 2.0, 3.0, 4.0);
        assert_eq!((w.x, w.y, w.z, w.w), (1.0, 2.0, 3.0, 4.0));
    }

    #[test]
    fn dot_product_accumulates_in_reference_order() {
        // 2^24 + 1 rounds back to 2^24 in f32, so the reference's
        // ((t1 + t2) + t3) yields 0 while any reordering yields 1. This
        // input distinguishes the accumulation order.
        let a = Vector3::new(16_777_216.0, 1.0, -16_777_216.0);
        let b = Vector3::new(1.0, 1.0, 1.0);
        let expected = ((a.x * b.x) + (a.y * b.y)) + (a.z * b.z);
        assert_eq!(a.dot_product(&b), expected);
        assert_eq!(expected, 0.0);
        let other = (a.x * b.x) + ((a.y * b.y) + (a.z * b.z));
        assert_eq!(other, 1.0);
        assert_ne!(expected, other, "test input must distinguish sum order");
    }

    #[test]
    fn cross_product_matches_reference_formulas() {
        let i = Vector3::I;
        let j = Vector3::J;
        let out = Vector3::ZERO.cross_product(&i, &j);
        assert_eq!(out, Vector3::K);
        let out = Vector3::ZERO.cross_product(&j, &i);
        assert_eq!(out, -Vector3::K);
        // Pinned scalar cross for Vector2.
        assert_eq!(
            Vector2::new(1.0, 0.0).cross_product(&Vector2::new(0.0, 1.0)),
            1.0
        );
    }

    #[test]
    fn normalise_leaves_almost_zero_untouched() {
        let mut v = Vector3::ZERO;
        let len = v.normalise();
        assert_eq!(len, 0.0);
        assert_eq!(v, Vector3::ZERO, "zero vector must not become NaN");

        let mut tiny = Vector3::new(1e-10, 0.0, 0.0);
        let len = tiny.normalise();
        assert_eq!(
            tiny,
            Vector3::new(1e-10, 0.0, 0.0),
            "length below 1e-8 is left as-is"
        );
        assert!((len - 1e-10).abs() < f32::EPSILON);
    }

    #[test]
    fn normalise_scales_by_reciprocal() {
        let mut v = Vector3::new(3.0, 0.0, 4.0);
        let len = v.normalise();
        assert_eq!(len, 5.0);
        assert_eq!(v, Vector3::new(0.6, 0.0, 0.8));
    }

    #[test]
    fn distance_widens_to_double_sqrt() {
        let a = Vector3::new(0.0, 0.0, 0.0);
        let b = Vector3::new(3.0, 4.0, 0.0);
        assert_eq!(a.distance(&b), 5.0);
        // The pinned formula: f64 square root, truncated to f32.
        let expected = ((3.0f64 * 3.0) + (4.0 * 4.0)).sqrt() as f32;
        let v = Vector3::new(3.0, 4.0, 0.0);
        assert_eq!(v.distance(&Vector3::ZERO), expected);
    }

    #[test]
    fn lerp_uses_reference_formula() {
        let a = Vector3::new(0.0, 0.0, 0.0);
        let b = Vector3::new(10.0, -4.0, 6.0);
        let t = 0.25;
        let r = Vector3::lerp(&a, &b, t);
        let expected = Vector3::new(
            (a.x * (1.0 - t)) + (b.x * t),
            (a.y * (1.0 - t)) + (b.y * t),
            (a.z * (1.0 - t)) + (b.z * t),
        );
        assert_eq!(r, expected);
        assert_eq!(r, Vector3::new(2.5, -1.0, 1.5));
    }

    #[test]
    fn direction_angles_follow_left_handed_convention() {
        // +Z forward: yaw 0, pitch 0.
        let fwd = Vector3::new(0.0, 0.0, 1.0);
        assert_eq!(fwd.yaw(), 0.0);
        assert_eq!(fwd.pitch(), 0.0);
        // +X: yaw +90 degrees.
        let right = Vector3::new(1.0, 0.0, 0.0);
        assert!((right.yaw() - std::f32::consts::FRAC_PI_2).abs() < 1e-6);
        // +Y up: pitch -90 degrees.
        let up = Vector3::new(0.0, 1.0, 0.0);
        assert!((up.pitch() + std::f32::consts::FRAC_PI_2).abs() < 1e-6);
    }

    #[test]
    fn set_pitch_yaw_roundtrips_through_angles() {
        let mut v = Vector3::ZERO;
        let pitch = 0.3f32;
        let yaw = -1.2f32;
        v.set_pitch_yaw(pitch, yaw);
        assert!((v.yaw() - yaw).abs() < 1e-5, "yaw roundtrip");
        assert!((v.pitch() - pitch).abs() < 1e-5, "pitch roundtrip");
        assert!((v.length() - 1.0).abs() < 1e-6, "unit length");
    }

    #[test]
    fn vector3_streams_as_three_little_endian_floats() {
        // Pinned wire format from vector3.cpp operator<<: x, y, z as f32 LE.
        let mut s = crate::stream::BinaryStream::new();
        Vector3::new(-1.5, 2.0, 3.25).to_stream_bytes(&mut s);
        let mut expected = crate::stream::BinaryStream::new();
        expected.write_f32(-1.5).write_f32(2.0).write_f32(3.25);
        assert_eq!(s.as_bytes(), expected.as_bytes());
        assert_eq!(s.as_bytes().len(), VECTOR3_STREAM_SIZE);
        assert_eq!(s.as_bytes()[4], 0x00, "2.0f32 LE second byte");

        s.rewind();
        let v = Vector3::from_stream_bytes(&mut s).unwrap();
        assert_eq!(v, Vector3::new(-1.5, 2.0, 3.25));
    }

    #[test]
    fn unit_vector_angle_is_acos_of_clamped_dot() {
        let a = Vector3::I;
        let b = Vector3::K;
        assert!((a.get_unit_vector_angle(&b) - std::f32::consts::FRAC_PI_2).abs() < 1e-6);
        // Opposite vectors clamp to 1.0 to avoid NaN from rounding.
        assert_eq!(a.get_unit_vector_angle(&(-a)), std::f32::consts::PI);
    }

    #[test]
    fn equality_is_bit_exact_not_epsilon() {
        let a = Vector3::new(1.0, 2.0, 3.0);
        let mut b = a;
        b.z += 1e-6; // more than one ulp of 3.0 (≈2.4e-7)
        assert_ne!(a, b, "reference isEqual is exact, not epsilon");
        let c = Vector3::new(1.0, 2.0, 3.0);
        assert_eq!(a, c);
    }

    #[test]
    fn division_multiplies_by_reciprocal() {
        let v = Vector3::new(1.0, 2.0, 4.0) / 2.0;
        assert_eq!(v, Vector3::new(0.5, 1.0, 2.0));
        // Pin the reciprocal-multiply form: same bits as (1.f/s) * c.
        let x = 1.0f32;
        let s = 7.0f32;
        let lhs = Vector3::new(x, 0.0, 0.0) / s;
        let rhs = Vector3::new((1.0f32 / s) * x, 0.0, 0.0);
        assert_eq!(lhs, rhs);
    }

    #[test]
    fn clamp_branch_order() {
        assert_eq!(clamp(-1.0, 0.5, 1.0), 0.5);
        assert_eq!(clamp(-1.0, -2.0, 1.0), -1.0);
        assert_eq!(clamp(-1.0, 2.0, 1.0), 1.0);
    }

    #[test]
    fn vector4_normalise_matches_reference() {
        let mut v = Vector4::new(1.0, 1.0, 1.0, 1.0);
        let len = v.normalise();
        assert!((len - 2.0).abs() < 1e-6);
        assert!((v.length() - 1.0).abs() < 1e-6);
        let mut zero = Vector4::ZERO;
        zero.normalise();
        assert_eq!(zero, Vector4::ZERO);
    }

    #[test]
    fn almost_zero_matches_reference_bounds() {
        assert!(almost_zero(0.0, 1e-8));
        assert!(almost_zero(9e-9, 1e-8));
        assert!(!almost_zero(1e-8, 1e-8), "boundary is exclusive both ends");
        assert!(!almost_zero(-1e-8, 1e-8));
    }

    /// `almostEqual` (`stdmf.hpp:606-609`): `fabsf(f1-f2) < 0.0004`.
    #[test]
    fn almost_equal_uses_the_reference_epsilon() {
        assert!(almost_equal(1.0, 1.0 + 3e-4));
        assert!(!almost_equal(1.0, 1.0 + 5e-4), "boundary is exclusive");
        assert!(!almost_equal(1.0, 2.0));
        assert!(almost_equal(-2.0, -2.0));
    }

    #[test]
    fn vector2_api_matches_reference() {
        let a = Vector2::new(3.0, 4.0);
        assert_eq!(a.dot_product(&Vector2::new(1.0, 2.0)), 11.0);
        assert_eq!(a.length_squared(), 25.0);
        assert_eq!(a.length(), 5.0);
        // Zero length short-circuits to 0 instead of sqrt(0).
        assert_eq!(Vector2::ZERO.length(), 0.0);

        let mut n = a;
        let len = n.normalise();
        assert_eq!(len, 5.0);
        assert_eq!(n, Vector2::new(0.6, 0.8));
        let mut tiny = Vector2::new(1e-10, 0.0);
        tiny.normalise();
        assert_eq!(tiny, Vector2::new(1e-10, 0.0), "below 1e-8 untouched");

        let mut s = Vector2::new(1.0, -2.0);
        s.scale(3.0);
        assert_eq!(s, Vector2::new(3.0, -6.0));
    }

    #[test]
    fn vector3_length_squared_and_distance_squared() {
        let v = Vector3::new(1.0, 2.0, 2.0);
        assert_eq!(v.length_squared(), 9.0);
        assert_eq!(
            v.distance_squared(&Vector3::new(0.0, 0.0, 0.0)),
            9.0,
            "distanceSquared uses lengthSquared of the delta"
        );
        assert_eq!(Vector3::ZERO.distance_squared(&Vector3::ZERO), 0.0);
    }

    #[test]
    fn vector2_operators_follow_the_reference_forms() {
        // operator+ (vector2.hpp:69): componentwise.
        let sum = Vector2::new(1.0, 2.0) + Vector2::new(3.0, -4.0);
        assert_eq!(sum, Vector2::new(4.0, -2.0));
        // operator- componentwise.
        assert_eq!(
            Vector2::new(3.0, -4.0) - Vector2::new(1.0, 2.0),
            Vector2::new(2.0, -6.0)
        );
        // operator* componentwise scale.
        assert_eq!(Vector2::new(1.0, -2.0) * 2.0, Vector2::new(2.0, -4.0));
        // operator/= (vector2.ipp:221-226): a reciprocal multiply, not a
        // per-component division — pinned by the 3.0 case (1/3 rounds the
        // same here, but the operation shape is pinned anyway).
        let q = Vector2::new(3.0, 6.0) / 3.0;
        assert_eq!(q, Vector2::new(1.0, 2.0));
        let manual = Vector2::new(3.0, 6.0) * (1.0f32 / 3.0);
        assert_eq!(q, manual);
    }
}
