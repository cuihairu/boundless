//! BW-01-01-04 matrices, following the reference scalar fallback path in
//! `lib/math/matrix.cpp` / `matrix.ipp`.
//!
//! Layout is row-major `m[row][col]`, translation lives in row 3, and vectors
//! transform as row vectors: `v' = v · M`. The default-constructed matrix is
//! the *zero* matrix, not the identity.

use super::vector::{Vector3, Vector4, clamp};

#[derive(Debug, Clone, Copy, PartialEq)]
#[repr(C)]
pub struct Matrix {
    /// `m[row][col]`, 16 f32s in memory order.
    pub m: [[f32; 4]; 4],
}

impl Default for Matrix {
    /// Reference default construction produces the zero matrix
    /// (`matrix.ipp:18-22`).
    fn default() -> Self {
        Self { m: [[0.0; 4]; 4] }
    }
}

impl Matrix {
    pub const fn zero() -> Self {
        Self { m: [[0.0; 4]; 4] }
    }

    pub fn identity() -> Self {
        let mut out = Self::zero();
        out.set_identity();
        out
    }

    pub fn set_identity(&mut self) {
        self.set_scale(1.0, 1.0, 1.0);
    }

    pub fn set_scale(&mut self, x: f32, y: f32, z: f32) {
        self.m[0][0] = x;
        self.m[0][1] = 0.0;
        self.m[0][2] = 0.0;
        self.m[0][3] = 0.0;

        self.m[1][0] = 0.0;
        self.m[1][1] = y;
        self.m[1][2] = 0.0;
        self.m[1][3] = 0.0;

        self.m[2][0] = 0.0;
        self.m[2][1] = 0.0;
        self.m[2][2] = z;
        self.m[2][3] = 0.0;

        self.m[3][0] = 0.0;
        self.m[3][1] = 0.0;
        self.m[3][2] = 0.0;
        self.m[3][3] = 1.0;
    }

    pub fn set_translate(&mut self, x: f32, y: f32, z: f32) {
        self.set_identity();
        self.m[3][0] = x;
        self.m[3][1] = y;
        self.m[3][2] = z;
    }

    /// Transform a point: row-vector product including the translation row
    /// (`matrix.ipp:190-222`).
    pub fn apply_point(&self, v: &Vector3) -> Vector3 {
        Vector3::new(
            (v.x * self.m[0][0]) + (v.y * self.m[1][0]) + (v.z * self.m[2][0]) + self.m[3][0],
            (v.x * self.m[0][1]) + (v.y * self.m[1][1]) + (v.z * self.m[2][1]) + self.m[3][1],
            (v.x * self.m[0][2]) + (v.y * self.m[1][2]) + (v.z * self.m[2][2]) + self.m[3][2],
        )
    }

    /// Transform a direction: the same product without the translation row
    /// (`matrix.ipp:278-359`).
    pub fn apply_vector(&self, v: &Vector3) -> Vector3 {
        Vector3::new(
            (v.x * self.m[0][0]) + (v.y * self.m[1][0]) + (v.z * self.m[2][0]),
            (v.x * self.m[0][1]) + (v.y * self.m[1][1]) + (v.z * self.m[2][1]),
            (v.x * self.m[0][2]) + (v.y * self.m[1][2]) + (v.z * self.m[2][2]),
        )
    }

    /// The `axis`-th basis vector is stored as a row, returned as a Vector3
    /// (`matrix.ipp:367-371` reinterprets `m[axis]`).
    pub fn apply_to_unit_axis_vector(&self, axis: usize) -> Vector3 {
        let row = &self.m[axis];
        Vector3::new(row[0], row[1], row[2])
    }

    /// The translation row (`matrix.ipp:378-382`).
    pub fn apply_to_origin(&self) -> Vector3 {
        Vector3::new(self.m[3][0], self.m[3][1], self.m[3][2])
    }

    /// Transform a full Vector4 point including the w row
    /// (`matrix.ipp:234-248`, scalar branch).
    pub fn apply_v4_point(&self, v: &Vector4) -> Vector4 {
        Vector4::new(
            (v.x * self.m[0][0])
                + (v.y * self.m[1][0])
                + (v.z * self.m[2][0])
                + (v.w * self.m[3][0]),
            (v.x * self.m[0][1])
                + (v.y * self.m[1][1])
                + (v.z * self.m[2][1])
                + (v.w * self.m[3][1]),
            (v.x * self.m[0][2])
                + (v.y * self.m[1][2])
                + (v.z * self.m[2][2])
                + (v.w * self.m[3][2]),
            (v.x * self.m[0][3])
                + (v.y * self.m[1][3])
                + (v.z * self.m[2][3])
                + (v.w * self.m[3][3]),
        )
    }

    /// View matrix from position/direction/up (`matrix.cpp:695-718`). The
    /// basis vectors land in *columns*: col0=Right, col1=Up, col2=Direction.
    /// The reference leaves `m[0][3]`/`m[1][3]`/`m[2][3]` untouched, so a
    /// caller must start from a zero matrix for a clean result.
    pub fn look_at(&mut self, position: &Vector3, direction: &Vector3, up: &Vector3) {
        let mut zaxis = *direction;
        zaxis.normalise();
        let mut xaxis = Vector3::new(0.0, 0.0, 0.0);
        xaxis = xaxis.cross_product(up, &zaxis);
        xaxis.normalise();
        let mut yaxis = Vector3::new(0.0, 0.0, 0.0);
        yaxis = yaxis.cross_product(&zaxis, &xaxis);

        self.m[0][0] = xaxis.x;
        self.m[1][0] = xaxis.y;
        self.m[2][0] = xaxis.z;
        self.m[3][0] = 0.0;

        self.m[0][1] = yaxis.x;
        self.m[1][1] = yaxis.y;
        self.m[2][1] = yaxis.z;
        self.m[3][1] = 0.0;

        self.m[0][2] = zaxis.x;
        self.m[1][2] = zaxis.y;
        self.m[2][2] = zaxis.z;
        self.m[3][2] = 0.0;

        self.m[3][0] = -position.dot_product(&xaxis);
        self.m[3][1] = -position.dot_product(&yaxis);
        self.m[3][2] = -position.dot_product(&zaxis);
        self.m[3][3] = 1.0;
    }

    /// Mirror test: `cross(row0, row2) · row1 > 0` (`matrix.cpp:129-132`).
    pub fn is_mirrored(&self) -> bool {
        let r0 = Vector3::new(self.m[0][0], self.m[0][1], self.m[0][2]);
        let r1 = Vector3::new(self.m[1][0], self.m[1][1], self.m[1][2]);
        let r2 = Vector3::new(self.m[2][0], self.m[2][1], self.m[2][2]);
        let v = Vector3::new(0.0, 0.0, 0.0);
        let v = v.cross_product(&r0, &r2);
        v.dot_product(&r1) > 0.0
    }

    /// Left-handed orthographic projection (`matrix.ipp:563-575`).
    pub fn orthogonal_projection(&mut self, w: f32, h: f32, zn: f32, zf: f32) {
        self.m[0] = [2.0 / w, 0.0, 0.0, 0.0];
        self.m[1] = [0.0, 2.0 / h, 0.0, 0.0];
        self.m[2] = [0.0, 0.0, 1.0 / (zf - zn), 0.0];
        self.m[3] = [0.0, 0.0, zn / (zn - zf), 1.0];
    }

    /// Left-handed perspective projection (`matrix.ipp:579-611`).
    pub fn perspective_projection(&mut self, fov: f32, aspect: f32, near: f32, far: f32) {
        let cot = 1.0 / (fov * 0.5).tan();
        let rcp = 1.0 / (far - near);

        self.m[0] = [cot / aspect, 0.0, 0.0, 0.0];
        self.m[1] = [0.0, cot, 0.0, 0.0];
        self.m[2] = [0.0, 0.0, rcp * far, 1.0];
        self.m[3] = [0.0, 0.0, -(rcp * far * near), 0.0];
    }

    /// Set only the translation row (`matrix.ipp:614-619`): the linear part
    /// is preserved, unlike `set_translate`.
    pub fn translation(&mut self, v: &Vector3) {
        self.m[3][0] = v.x;
        self.m[3][1] = v.y;
        self.m[3][2] = v.z;
        self.m[3][3] = 1.0;
    }

    /// `m[col&3][row&3]`, matching the reference `operator[](int)`
    /// reinterpretation (`script_math.hpp:209`).
    pub fn get_element(&self, col: usize, row: usize) -> f32 {
        self.m[col & 3][row & 3]
    }

    /// `m[col&3][row&3] = value` (`script_math.hpp:206`).
    pub fn set_element(&mut self, col: usize, row: usize, value: f32) {
        self.m[col & 3][row & 3] = value;
    }

    pub fn row(&self, i: usize) -> Vector4 {
        Vector4::new(self.m[i][0], self.m[i][1], self.m[i][2], self.m[i][3])
    }

    pub fn column(&self, i: usize) -> Vector4 {
        Vector4::new(self.m[0][i], self.m[1][i], self.m[2][i], self.m[3][i])
    }

    /// `this = m1 * m2`, expanded in `k = 0..4` order (`matrix.cpp:348-405`).
    pub fn multiply(&mut self, m1: &Self, m2: &Self) {
        let mut out = Self::zero();
        for i in 0..4 {
            for j in 0..4 {
                let mut sum = 0.0f32;
                for k in 0..4 {
                    sum += m1.m[i][k] * m2.m[k][j];
                }
                out.m[i][j] = sum;
            }
        }
        *self = out;
    }

    /// `this = m * this`.
    pub fn pre_multiply(&mut self, m: &Self) {
        let tmp = *self;
        self.multiply(m, &tmp);
    }

    /// `this = this * m`.
    pub fn post_multiply(&mut self, m: &Self) {
        let tmp = *self;
        self.multiply(&tmp, m);
    }

    pub fn set_rotate_x(&mut self, angle: f32) {
        let c = angle.cos();
        let s = angle.sin();
        self.set_identity();
        self.m[1][1] = c;
        self.m[1][2] = s;
        self.m[2][1] = -s;
        self.m[2][2] = c;
    }

    pub fn set_rotate_y(&mut self, angle: f32) {
        let c = angle.cos();
        let s = angle.sin();
        self.set_identity();
        self.m[0][0] = c;
        self.m[0][2] = -s;
        self.m[2][0] = s;
        self.m[2][2] = c;
    }

    pub fn set_rotate_z(&mut self, angle: f32) {
        let c = angle.cos();
        let s = angle.sin();
        self.set_identity();
        self.m[0][0] = c;
        self.m[0][1] = s;
        self.m[1][0] = -s;
        self.m[1][1] = c;
    }

    pub fn pre_rotate_x(&mut self, angle: f32) {
        let mut rot = Self::zero();
        rot.set_rotate_x(angle);
        self.pre_multiply(&rot);
    }

    pub fn pre_rotate_y(&mut self, angle: f32) {
        let mut rot = Self::zero();
        rot.set_rotate_y(angle);
        self.pre_multiply(&rot);
    }

    pub fn pre_rotate_z(&mut self, angle: f32) {
        let mut rot = Self::zero();
        rot.set_rotate_z(angle);
        self.pre_multiply(&rot);
    }

    pub fn post_rotate_x(&mut self, angle: f32) {
        let mut rot = Self::zero();
        rot.set_rotate_x(angle);
        self.post_multiply(&rot);
    }

    pub fn post_rotate_y(&mut self, angle: f32) {
        let mut rot = Self::zero();
        rot.set_rotate_y(angle);
        self.post_multiply(&rot);
    }

    pub fn post_rotate_z(&mut self, angle: f32) {
        let mut rot = Self::zero();
        rot.set_rotate_z(angle);
        self.post_multiply(&rot);
    }

    /// Euler construction: `setRotateY(yaw)`, then `preRotateX(pitch)`, then
    /// `preRotateZ(roll)` (`matrix.cpp:562-608`), algebraically
    /// `M = Rz(roll) · Rx(pitch) · Ry(yaw)`.
    pub fn set_rotate_ypr(&mut self, yaw: f32, pitch: f32, roll: f32) {
        self.set_rotate_y(yaw);
        self.pre_rotate_x(pitch);
        self.pre_rotate_z(roll);
    }

    /// Euler construction of the inverse rotation (`matrix.cpp:195-242`).
    pub fn set_rotate_inverse(&mut self, yaw: f32, pitch: f32, roll: f32) {
        self.set_rotate_ypr(yaw, pitch, roll);
        self.invert_orthonormal();
    }

    /// Orthonormal inverse: transpose the 3x3 block and negate the
    /// translation via dot products (`matrix.cpp:156-177`).
    pub fn invert_orthonormal(&mut self) {
        let mut out = Self::zero();
        out.m[3][3] = 1.0;
        for i in 0..3 {
            for j in 0..3 {
                out.m[i][j] = self.m[j][i];
            }
        }
        // new translation row = -(rotated origin)
        let orig = self.apply_to_origin();
        let inv = Vector3::new(
            -(orig.x * self.m[0][0] + orig.y * self.m[0][1] + orig.z * self.m[0][2]),
            -(orig.x * self.m[1][0] + orig.y * self.m[1][1] + orig.z * self.m[1][2]),
            -(orig.x * self.m[2][0] + orig.y * self.m[2][1] + orig.z * self.m[2][2]),
        );
        out.m[3][0] = inv.x;
        out.m[3][1] = inv.y;
        out.m[3][2] = inv.z;
        *self = out;
    }

    /// Yaw extraction (`matrix.ipp:638-684`): normalised z axis row, then
    /// `atan2f(zdir.x, zdir.z)`.
    pub fn yaw(&self) -> f32 {
        let mut zdir = self.apply_to_unit_axis_vector(2);
        zdir.normalise();
        zdir.x.atan2(zdir.z)
    }

    /// Pitch extraction: `−asinf(clamp(−1, zdir.y, +1))` on the normalised z
    /// axis row.
    pub fn pitch(&self) -> f32 {
        let mut zdir = self.apply_to_unit_axis_vector(2);
        zdir.normalise();
        -clamp(-1.0, zdir.y, 1.0).asin()
    }

    /// Roll extraction: `atan2f(xdir.y, ydir.y)` on the normalised x/y rows.
    pub fn roll(&self) -> f32 {
        let mut xdir = self.apply_to_unit_axis_vector(0);
        let mut ydir = self.apply_to_unit_axis_vector(1);
        xdir.normalise();
        ydir.normalise();
        xdir.y.atan2(ydir.y)
    }

    /// `preTranslateBy`: rotate then add (`matrix.ipp:132-149`).
    pub fn pre_translate_by(&mut self, v: &Vector3) {
        let rotated = self.apply_vector(v);
        let origin = self.apply_to_origin();
        let t = origin + rotated;
        self.m[3][0] = t.x;
        self.m[3][1] = t.y;
        self.m[3][2] = t.z;
    }

    /// `postTranslateBy`: direct add.
    pub fn post_translate_by(&mut self, v: &Vector3) {
        let origin = self.apply_to_origin();
        let t = origin + *v;
        self.m[3][0] = t.x;
        self.m[3][1] = t.y;
        self.m[3][2] = t.z;
    }

    /// Quaternion → rotation matrix entries exactly as `matrix.cpp:517-555`.
    pub fn set_rotate_quat(&mut self, q: &super::quaternion::Quaternion) {
        let xx = 2.0 * q.x * q.x;
        let xy = 2.0 * q.x * q.y;
        let xz = 2.0 * q.x * q.z;
        let yy = 2.0 * q.y * q.y;
        let yz = 2.0 * q.y * q.z;
        let zz = 2.0 * q.z * q.z;
        let wx = 2.0 * q.w * q.x;
        let wy = 2.0 * q.w * q.y;
        let wz = 2.0 * q.w * q.z;

        self.set_identity();
        self.m[0][0] = 1.0 - (yy + zz);
        self.m[1][0] = xy - wz;
        self.m[2][0] = xz + wy;

        self.m[0][1] = xy + wz;
        self.m[1][1] = 1.0 - (xx + zz);
        self.m[2][1] = yz - wx;

        self.m[0][2] = xz - wy;
        self.m[1][2] = yz + wx;
        self.m[2][2] = 1.0 - (xx + yy);

        self.m[0][3] = 0.0;
        self.m[1][3] = 0.0;
        self.m[2][3] = 0.0;
        self.m[3][0] = 0.0;
        self.m[3][1] = 0.0;
        self.m[3][2] = 0.0;
        self.m[3][3] = 1.0;
    }

    /// Full 3x4 inverse via the 3x3 determinant; returns false for singular
    /// input (`matrix.cpp:616-658`).
    pub fn invert(&mut self) -> bool {
        let det = self.determinant();
        if det == 0.0 {
            return false;
        }
        let mut out = Self::zero();
        let inv_det = 1.0 / det;
        let (a, b, c) = (self.m[0][0], self.m[0][1], self.m[0][2]);
        let (d, e, f) = (self.m[1][0], self.m[1][1], self.m[1][2]);
        let (g, h, i) = (self.m[2][0], self.m[2][1], self.m[2][2]);

        // Row-major layout follows `matrix.cpp:631-639` exactly: the cofactor of
        // (c, r) lands in m[r][c].
        out.m[0][0] = (e * i - f * h) * inv_det;
        out.m[0][1] = (c * h - b * i) * inv_det;
        out.m[0][2] = (b * f - c * e) * inv_det;
        out.m[1][0] = (f * g - d * i) * inv_det;
        out.m[1][1] = (a * i - c * g) * inv_det;
        out.m[1][2] = (c * d - a * f) * inv_det;
        out.m[2][0] = (d * h - e * g) * inv_det;
        out.m[2][1] = (b * g - a * h) * inv_det;
        out.m[2][2] = (a * e - b * d) * inv_det;

        // Translation: negate and transform by the inverted linear part.
        let orig = self.apply_to_origin();
        out.m[3][0] = -(out.m[0][0] * orig.x + out.m[1][0] * orig.y + out.m[2][0] * orig.z);
        out.m[3][1] = -(out.m[0][1] * orig.x + out.m[1][1] * orig.y + out.m[2][1] * orig.z);
        out.m[3][2] = -(out.m[0][2] * orig.x + out.m[1][2] * orig.y + out.m[2][2] * orig.z);
        out.m[3][3] = 1.0;
        *self = out;
        true
    }

    /// 3x3 determinant of the linear part (`matrix.ipp:66-81`).
    pub fn determinant(&self) -> f32 {
        self.m[0][0] * (self.m[1][1] * self.m[2][2] - self.m[1][2] * self.m[2][1])
            - self.m[1][0] * (self.m[0][1] * self.m[2][2] - self.m[0][2] * self.m[2][1])
            + self.m[2][0] * (self.m[0][1] * self.m[1][2] - self.m[0][2] * self.m[1][1])
    }

    /// Componentwise tolerance comparison for tests and assertions. The
    /// reference `operator==` stays bit-exact; this mirrors `almostEqual`.
    pub fn almost(&self, other: &Self, epsilon: f32) -> bool {
        for r in 0..4 {
            for c in 0..4 {
                if (self.m[r][c] - other.m[r][c]).abs() >= epsilon {
                    return false;
                }
            }
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_zero_not_identity() {
        let m = Matrix::default();
        assert_eq!(m, Matrix::zero());
        assert_eq!(m.m[0][0], 0.0);
        assert_eq!(m.m[3][3], 0.0);
    }

    #[test]
    fn identity_and_translate_fill_expected_cells() {
        let i = Matrix::identity();
        assert_eq!(i.m[0][0], 1.0);
        assert_eq!(i.m[1][1], 1.0);
        assert_eq!(i.m[2][2], 1.0);
        assert_eq!(i.m[3][3], 1.0);

        let t = {
            let mut t = Matrix::zero();
            t.set_translate(3.0, 4.0, 5.0);
            t
        };
        assert_eq!(t.m[3][0], 3.0);
        assert_eq!(t.m[3][1], 4.0);
        assert_eq!(t.m[3][2], 5.0);
        assert_eq!(t.m[0][0], 1.0, "setTranslate also resets the linear part");
    }

    #[test]
    fn apply_point_is_row_vector_product_with_translation() {
        let mut m = Matrix::zero();
        m.set_translate(1.0, 2.0, 3.0);
        let v = Vector3::new(10.0, 20.0, 30.0);
        assert_eq!(m.apply_point(&v), Vector3::new(11.0, 22.0, 33.0));

        // Pinned accumulation order: v0*m00 + v1*m10 + v2*m20 + m30.
        let mut r = Matrix::zero();
        r.set_scale(1.0, 1.0, 1.0);
        r.m[0][0] = 65536.0;
        r.m[1][0] = 1.0;
        let v2 = Vector3::new(1.0, 65536.0, 0.0);
        let expected = (v2.x * r.m[0][0]) + (v2.y * r.m[1][0]) + (v2.z * r.m[2][0]) + r.m[3][0];
        assert_eq!(r.apply_point(&v2).x, expected);
    }

    #[test]
    fn apply_vector_excludes_translation() {
        let mut m = Matrix::zero();
        m.set_translate(1.0, 2.0, 3.0);
        let v = Vector3::new(10.0, 20.0, 30.0);
        assert_eq!(m.apply_vector(&v), v);
    }

    #[test]
    fn apply_to_origin_is_the_translation_row() {
        let mut t = Matrix::zero();
        t.set_translate(7.0, 8.0, 9.0);
        assert_eq!(t.apply_to_origin(), Vector3::new(7.0, 8.0, 9.0));
    }

    #[test]
    fn row_column_accessors() {
        let mut m = Matrix::zero();
        m.set_translate(1.0, 2.0, 3.0);
        assert_eq!(m.row(3), Vector4::new(1.0, 2.0, 3.0, 1.0));
        assert_eq!(m.column(0), Vector4::new(1.0, 0.0, 0.0, 1.0));
    }

    #[test]
    fn rotate_x_maps_y_to_z_for_positive_angles() {
        let a = std::f32::consts::FRAC_PI_2;
        let mut rot = Matrix::zero();
        rot.set_identity();
        rot.m[1][1] = a.cos();
        rot.m[1][2] = a.sin();
        rot.m[2][1] = -a.sin();
        rot.m[2][2] = a.cos();
        // Rotating +J (0,1,0) by +90° about X moves it to +K (0,0,1): Y→Z.
        let mapped = rot.apply_vector(&Vector3::J);
        assert!(crate::math::almost_zero(mapped.x, 1e-6));
        assert!(
            crate::math::almost_zero(mapped.y, 1e-6),
            "y component vanishes"
        );
        assert!(
            crate::math::almost_zero(mapped.z - 1.0, 1e-6),
            "z component becomes +1"
        );
    }

    #[test]
    fn rotate_y_maps_z_to_x_for_positive_angles() {
        let a = std::f32::consts::FRAC_PI_2;
        let mut rot = Matrix::zero();
        rot.set_identity();
        rot.m[0][0] = a.cos();
        rot.m[0][2] = -a.sin();
        rot.m[2][0] = a.sin();
        rot.m[2][2] = a.cos();
        let mapped = rot.apply_vector(&Vector3::K);
        assert!((mapped.x - 1.0).abs() < 1e-6, "Z→X for positive yaw");
        assert!(mapped.z.abs() < 1e-6);
    }

    #[test]
    fn rotate_z_maps_x_to_y_for_positive_angles() {
        let a = std::f32::consts::FRAC_PI_2;
        let mut rot = Matrix::zero();
        rot.set_identity();
        rot.m[0][0] = a.cos();
        rot.m[0][1] = a.sin();
        rot.m[1][0] = -a.sin();
        rot.m[1][1] = a.cos();
        let mapped = rot.apply_vector(&Vector3::I);
        assert!((mapped.y - 1.0).abs() < 1e-6, "X→Y for positive roll");
        assert!(mapped.x.abs() < 1e-6);
    }

    #[test]
    fn euler_order_is_rz_times_rx_times_ry() {
        // Build M = Rz·Rx·Ry manually and compare with set_rotate_ypr.
        let yaw = 0.7f32;
        let pitch = -0.3f32;
        let roll = 1.1f32;

        let mut m = Matrix::zero();
        m.set_rotate_ypr(yaw, pitch, roll);

        let mut expected = Matrix::zero();
        expected.set_rotate_z(roll);
        let mut rx = Matrix::zero();
        rx.set_identity();
        {
            let c = pitch.cos();
            let s = pitch.sin();
            rx.m[1][1] = c;
            rx.m[1][2] = s;
            rx.m[2][1] = -s;
            rx.m[2][2] = c;
        }
        let mut ry = Matrix::zero();
        ry.set_identity();
        {
            let c = yaw.cos();
            let s = yaw.sin();
            ry.m[0][0] = c;
            ry.m[0][2] = -s;
            ry.m[2][0] = s;
            ry.m[2][2] = c;
        }
        // Algebraic product: Rz * Rx * Ry. apply in row-vector order:
        // the composition applied to v is Rz(Rx(Ry(v))).
        let mut tmp = Matrix::zero();
        tmp.multiply(&rx, &ry);
        let mut prod = Matrix::zero();
        prod.multiply(&expected, &tmp);
        assert_eq!(m, prod);
    }

    #[test]
    fn yaw_pitch_roll_extraction_roundtrip() {
        let yaw = 0.7f32;
        let pitch = -0.3f32;
        let roll = 1.1f32;
        let mut m = Matrix::zero();
        m.set_rotate_ypr(yaw, pitch, roll);
        assert!((m.yaw() - yaw).abs() < 1e-5, "yaw {} vs {}", m.yaw(), yaw);
        assert!(
            (m.pitch() - pitch).abs() < 1e-5,
            "pitch {} vs {}",
            m.pitch(),
            pitch
        );
        assert!(
            (m.roll() - roll).abs() < 1e-4,
            "roll {} vs {}",
            m.roll(),
            roll
        );
    }

    #[test]
    fn invert_orthonormal_roundtrips() {
        let mut m = Matrix::zero();
        m.set_rotate_ypr(0.3, 0.8, -0.4);
        m.post_translate_by(&Vector3::new(5.0, -6.0, 7.0));

        let mut inv = m;
        inv.invert_orthonormal();

        let p = Vector3::new(1.0, 2.0, 3.0);
        let roundtrip = inv.apply_point(&m.apply_point(&p));
        assert!(super::super::vector::almost_zero(roundtrip.x - p.x, 1e-5));
        assert!(super::super::vector::almost_zero(roundtrip.y - p.y, 1e-5));
        assert!(super::super::vector::almost_zero(roundtrip.z - p.z, 1e-5));
    }

    #[test]
    fn general_invert_singular_returns_false() {
        let mut m = Matrix::zero();
        m.m[0][0] = 1.0;
        assert!(!m.invert(), "zero 3x3 block is singular");

        let mut t = Matrix::zero();
        t.set_translate(1.0, 2.0, 3.0);
        assert!(t.invert());
        let p = t.apply_point(&Vector3::new(4.0, 5.0, 6.0));
        let back = {
            let mut inv = t;
            inv.invert();
            inv.apply_point(&p)
        };
        assert!((back.x - 4.0).abs() < 1e-5);
        assert!((back.y - 5.0).abs() < 1e-5);
        assert!((back.z - 6.0).abs() < 1e-5);
    }

    #[test]
    fn general_invert_matches_the_reference_adjugate() {
        // A rotation's linear part is not symmetric, so a transposed inverse
        // would not round-trip; this pins the reference's adjugate layout.
        let mut m = Matrix::zero();
        m.set_rotate_x(0.3);
        m.post_translate_by(&Vector3::new(1.0, -2.0, 3.0));

        let mut inv = m;
        assert!(inv.invert());

        for &p in &[Vector3::new(1.0, 2.0, 3.0), Vector3::new(-4.0, 0.5, 7.5)] {
            let back = inv.apply_point(&m.apply_point(&p));
            assert!((back.x - p.x).abs() < 1e-4, "x {}", back.x - p.x);
            assert!((back.y - p.y).abs() < 1e-4, "y {}", back.y - p.y);
            assert!((back.z - p.z).abs() < 1e-4, "z {}", back.z - p.z);
        }

        // Inverting twice returns the matrix it started from.
        let mut twice = inv;
        assert!(twice.invert());
        assert!(twice.almost(&m, 1e-5));
    }

    #[test]
    fn pre_and_post_multiply_order() {
        let mut a = Matrix::zero();
        a.set_translate(1.0, 0.0, 0.0);
        let mut b = Matrix::zero();
        b.set_translate(0.0, 1.0, 0.0);

        // post: this * m → translate by A then by B.
        let mut p = a;
        p.post_multiply(&b);
        assert_eq!(p.apply_point(&Vector3::ZERO), Vector3::new(1.0, 1.0, 0.0));

        // pre: m * this → order reversed.
        let mut q = a;
        q.pre_multiply(&b);
        let mut expected = Matrix::zero();
        expected.multiply(&b, &a);
        assert_eq!(q, expected);
    }

    #[test]
    fn translate_helpers() {
        let mut m = Matrix::zero();
        m.set_rotate_y(std::f32::consts::FRAC_PI_2);
        let v = Vector3::new(1.0, 0.0, 0.0);
        m.pre_translate_by(&v);
        // preTranslateBy rotates v then adds: +X rotated by +90° yaw lands on
        // −Z in this left-handed convention (Z→X implies X→−Z).
        let o = m.apply_to_origin();
        assert!(crate::math::almost_zero(o.x, 1e-6));
        assert!(crate::math::almost_zero(o.z + 1.0, 1e-6), "o.z = {}", o.z);

        let mut m2 = Matrix::zero();
        m2.set_identity();
        m2.post_translate_by(&v);
        assert_eq!(m2.apply_to_origin(), v);
    }

    #[test]
    fn pre_rotate_applies_rotation_before_the_existing_transform() {
        // preRotateX/Y/Z = preMultiply(setRotate*(a)): rotation happens
        // first in world space, then the existing matrix applies.
        let yaw = 1.1f32;

        let mut via_helper = Matrix::zero();
        via_helper.set_translate(1.0, 2.0, 3.0);
        via_helper.pre_rotate_y(yaw);

        let mut rot = Matrix::zero();
        rot.set_rotate_y(yaw);
        let mut via_multiply = Matrix::zero();
        via_multiply.set_translate(1.0, 2.0, 3.0);
        via_multiply.pre_multiply(&rot);

        assert_eq!(via_helper, via_multiply);

        // Same shape for X and Z.
        let mut px = Matrix::zero();
        px.set_translate(1.0, 2.0, 3.0);
        px.pre_rotate_x(0.4);
        let mut rx = Matrix::zero();
        rx.set_rotate_x(0.4);
        let mut expect_x = Matrix::zero();
        expect_x.set_translate(1.0, 2.0, 3.0);
        expect_x.pre_multiply(&rx);
        assert_eq!(px, expect_x);

        let mut pz = Matrix::zero();
        pz.set_translate(1.0, 2.0, 3.0);
        pz.pre_rotate_z(-0.9);
        let mut rz = Matrix::zero();
        rz.set_rotate_z(-0.9);
        let mut expect_z = Matrix::zero();
        expect_z.set_translate(1.0, 2.0, 3.0);
        expect_z.pre_multiply(&rz);
        assert_eq!(pz, expect_z);
    }

    #[test]
    fn post_rotate_applies_rotation_after_the_existing_transform() {
        // postRotateX/Y/Z = postMultiply(setRotate*(a)): the existing matrix
        // applies first, the rotation happens afterwards.
        let yaw = 1.1f32;

        let mut via_helper = Matrix::zero();
        via_helper.set_translate(1.0, 2.0, 3.0);
        via_helper.post_rotate_y(yaw);

        let mut rot = Matrix::zero();
        rot.set_rotate_y(yaw);
        let mut via_multiply = Matrix::zero();
        via_multiply.set_translate(1.0, 2.0, 3.0);
        via_multiply.post_multiply(&rot);

        assert_eq!(via_helper, via_multiply);

        // Same shape for X and Z.
        let mut px = Matrix::zero();
        px.set_translate(1.0, 2.0, 3.0);
        px.post_rotate_x(0.4);
        let mut rx = Matrix::zero();
        rx.set_rotate_x(0.4);
        let mut expect_x = Matrix::zero();
        expect_x.set_translate(1.0, 2.0, 3.0);
        expect_x.post_multiply(&rx);
        assert_eq!(px, expect_x);

        let mut pz = Matrix::zero();
        pz.set_translate(1.0, 2.0, 3.0);
        pz.post_rotate_z(-0.9);
        let mut rz = Matrix::zero();
        rz.set_rotate_z(-0.9);
        let mut expect_z = Matrix::zero();
        expect_z.set_translate(1.0, 2.0, 3.0);
        expect_z.post_multiply(&rz);
        assert_eq!(pz, expect_z);
    }

    #[test]
    fn almost_returns_false_on_first_mismatching_entry() {
        let mut a = Matrix::zero();
        a.set_identity();
        let mut b = a;
        b.m[2][1] += 1e-3;
        assert!(!a.almost(&b, 1e-4), "mismatch inside epsilon → false");
        assert!(a.almost(&b, 1e-2), "loose epsilon still matches");
        // Mismatch in the last row/column is found by the same loop.
        let mut c = a;
        c.m[3][3] += 1e-3;
        assert!(!a.almost(&c, 1e-4));
    }

    #[test]
    fn scale_sets_diagonal() {
        let mut m = Matrix::zero();
        m.set_scale(2.0, 3.0, 4.0);
        assert_eq!(
            m.apply_vector(&Vector3::new(1.0, 1.0, 1.0)),
            Vector3::new(2.0, 3.0, 4.0)
        );
        assert_eq!(m.m[3][3], 1.0);
    }

    #[test]
    fn set_rotate_inverse_is_inverse_rotation() {
        let yaw = 0.9f32;
        let pitch = 0.4f32;
        let roll = -0.2f32;
        let mut m = Matrix::zero();
        m.set_rotate_ypr(yaw, pitch, roll);
        let mut inv = Matrix::zero();
        inv.set_rotate_inverse(yaw, pitch, roll);
        let mut product = Matrix::zero();
        product.multiply(&inv, &m);
        // Row-vector convention: inv·M composed leaves basis rows as identity.
        let round = product.apply_vector(&Vector3::I);
        assert!(super::super::vector::almost_zero(round.x - 1.0, 1e-5));
        assert!(super::super::vector::almost_zero(round.y, 1e-5));
        assert!(super::super::vector::almost_zero(round.z, 1e-5));
    }

    #[test]
    fn determinant_of_identity_and_scale() {
        let i = Matrix::identity();
        assert_eq!(i.determinant(), 1.0);
        let mut s = Matrix::zero();
        s.set_scale(2.0, 3.0, 4.0);
        assert_eq!(s.determinant(), 24.0);
    }

    #[test]
    fn apply_v4_point_matches_apply_point_with_w_one() {
        let mut m = Matrix::zero();
        m.set_translate(3.0, 4.0, 5.0);
        let p = Vector3::new(1.0, 2.0, 3.0);
        let v4 = Vector4::new(p.x, p.y, p.z, 1.0);
        let via4 = m.apply_v4_point(&v4);
        let via3 = m.apply_point(&p);
        assert_eq!(via3.x, via4.x);
        assert_eq!(via3.y, via4.y);
        assert_eq!(via3.z, via4.z);
        assert_eq!(via4.w, 1.0);
    }

    #[test]
    fn look_at_builds_orthonormal_view() {
        let mut m = Matrix::zero();
        let pos = Vector3::new(0.0, 0.0, 0.0);
        let dir = Vector3::new(0.0, 0.0, 1.0);
        let up = Vector3::new(0.0, 1.0, 0.0);
        m.look_at(&pos, &dir, &up);
        // Straight-down-the-z view: identity rotation, zero translation.
        assert_eq!(m.m[0][0], 1.0);
        assert_eq!(m.m[1][1], 1.0);
        assert_eq!(m.m[2][2], 1.0);
        assert_eq!(m.m[3][0], 0.0);
        assert_eq!(m.m[3][1], 0.0);
        assert_eq!(m.m[3][2], 0.0);
        assert_eq!(m.m[3][3], 1.0);

        // Offset position shifts the translation by -pos·axis.
        let pos2 = Vector3::new(10.0, 0.0, 0.0);
        let mut m2 = Matrix::zero();
        m2.look_at(&pos2, &dir, &up);
        assert_eq!(m2.m[3][0], -10.0);
        assert_eq!(m2.m[3][1], 0.0);
        assert_eq!(m2.m[3][2], 0.0);
    }

    #[test]
    fn is_mirrored_flags_reflected_basis() {
        let i = Matrix::identity();
        assert!(!i.is_mirrored());
        // Negate the x basis row: right-handed becomes left-handed.
        let mut m = Matrix::zero();
        m.set_scale(-1.0, 1.0, 1.0);
        assert!(m.is_mirrored());
    }

    #[test]
    fn orthogonal_projection_fills_rows() {
        let mut m = Matrix::zero();
        m.orthogonal_projection(100.0, 50.0, 0.5, 100.5);
        assert_eq!(m.m[0][0], 2.0 / 100.0);
        assert_eq!(m.m[1][1], 2.0 / 50.0);
        assert_eq!(m.m[2][2], 1.0 / 100.0);
        assert_eq!(m.m[3][2], 0.5 / (0.5 - 100.5));
        assert_eq!(m.m[3][3], 1.0);
    }

    #[test]
    fn perspective_projection_fills_rows() {
        let mut m = Matrix::zero();
        m.perspective_projection(1.0, 1.5, 0.5, 100.5);
        let cot = 1.0 / 0.5f32.tan();
        let rcp = 1.0 / 100.0;
        assert!((m.m[0][0] - cot / 1.5).abs() < 1e-6);
        assert_eq!(m.m[1][1], cot);
        assert_eq!(m.m[2][2], rcp * 100.5);
        assert_eq!(m.m[2][3], 1.0);
        assert_eq!(m.m[3][2], -(rcp * 100.5 * 0.5));
        assert_eq!(m.m[3][3], 0.0);
    }

    #[test]
    fn translation_setter_preserves_linear_part() {
        let mut m = Matrix::zero();
        m.set_scale(2.0, 3.0, 4.0);
        m.translation(&Vector3::new(1.0, 2.0, 3.0));
        assert_eq!(m.m[3][0], 1.0);
        assert_eq!(m.m[3][1], 2.0);
        assert_eq!(m.m[3][2], 3.0);
        assert_eq!(m.m[3][3], 1.0);
        // Linear part untouched.
        assert_eq!(m.m[0][0], 2.0);
        assert_eq!(m.m[1][1], 3.0);
        assert_eq!(m.m[2][2], 4.0);
    }

    #[test]
    fn element_accessors_use_col_row_masking() {
        let mut m = Matrix::zero();
        m.set_element(3, 0, 1.0);
        m.set_element(3, 1, 2.0);
        m.set_element(3, 2, 4.0);
        assert_eq!(m.m[3][0], 1.0);
        assert_eq!(m.m[3][1], 2.0);
        assert_eq!(m.m[3][2], 4.0);
        assert_eq!(m.get_element(3, 0), 1.0);
        assert_eq!(m.get_element(3, 1), 2.0);
        assert_eq!(m.get_element(3, 2), 4.0);
        // Out-of-range indices wrap mod 4.
        m.set_element(5, 1, 9.0);
        assert_eq!(m.get_element(1, 1), 9.0);
    }
}
