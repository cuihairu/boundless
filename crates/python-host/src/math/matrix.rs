//! The `Math.Matrix` and `Math.Plane` classes, the reference's `PyMatrix`
//! (`lib/pyscript/script_math.cpp:140-890`) and `PyPlane`
//! (`:4379-5283`, class `script_math.hpp:508-527`).
//!
//! `PyMatrix` inherits both `MatrixProvider` and the raw `Matrix` value,
//! so the port separates them the same way: [`MatrixValue`] is the plain
//! 4x4 float block (`MatrixBase` on non-Windows is `float m[4][4]`,
//! `xp_math.hpp:222-235`; `EXT_MATH` is Windows-only, so the portable
//! bodies of `matrix.cpp`/`matrix.ipp` are the live code), and the
//! instance body only wraps one. The provider half -- what `set`,
//! `preMultiply`, `postMultiply` and the constructor snapshot through --
//! dispatches on the provider types [`super::provider`] creates.
//!
//! Two faithful quirks to note: `lookAt` writes `m[3][0]` twice and never
//! touches `m[0..3][3]` (`matrix.cpp:695-724`), and `invert` refuses only
//! an *exactly* zero determinant -- `isZero(float)` is `fabsf(f-0) <= 0.f`
//! (`stdmf_minimal.hpp:118-122`) -- leaving the matrix untouched on
//! refusal. Angles compute through doubles: every `sin`/`cos` call takes
//! the float argument as a double and truncates the result
//! (`(float)sin(angle)`, `matrix.cpp:438-440`).

use crate::binding::{self, TypeSpec};
use crate::capi;
use crate::pyref::PyRef;

use super::provider;
use super::vector::{parse_vector_like, vector_object};
use super::{
    arg_count_error, body, bool_object, check_type, coerce_float, coerce_uint, float_object,
    own_raw, raise_type_error, remember_type, takes_no_arguments, type_address,
};

use std::ffi::c_int;

/// The dotted type name of the matrix class.
const MATRIX_NAME: &str = "Math.Matrix";

/// The dotted type name of the plane class.
const PLANE_NAME: &str = "Math.Plane";

/// One angle as the reference accepts it: the parse truncates to float
/// first, then the `|a| <= 100` window applies
/// (`script_math.hpp:66-75`).
fn is_valid_angle(angle: f32) -> bool {
    (-100.0..=100.0).contains(&angle)
}

/// `almostZero` (`cstdmf/stdmf.hpp:616-619`): the open interval test.
fn almost_zero(value: f32, epsilon: f32) -> bool {
    -epsilon < value && value < epsilon
}

/// `Vector3::dotProduct` (`vector3.ipp:109-114`).
fn dot3(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

/// `Vector3::crossProduct` (`vector3.ipp:123-133`).
fn cross3(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [
        (a[1] * b[2]) - (a[2] * b[1]),
        (a[2] * b[0]) - (a[0] * b[2]),
        (a[0] * b[1]) - (a[1] * b[0]),
    ]
}

/// `Vector3::length` (`vector3.ipp:217-227`).
fn length3(v: [f32; 3]) -> f32 {
    (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt()
}

/// `Vector3::normalise` (`vector3.ipp:272-286`): divides by `1/length`
/// only when the length clears `almostZero(.., 1e-8)`; a near-zero vector
/// is left untouched.
fn normalise3(v: &mut [f32; 3]) {
    let length = length3(*v);
    if !almost_zero(length, 0.000_000_01) {
        let scale = 1.0_f32 / length;
        *v = [v[0] * scale, v[1] * scale, v[2] * scale];
    }
}

/// The plain 4x4 float block, the non-Windows `MatrixBase`
/// (`xp_math.hpp:222-235`): row-major, so the axis vectors are *rows*
/// (`applyToUnitAxisVector` reads `m[axis]`, `matrix.ipp:368-376`) and the
/// translation is `m[3]`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct MatrixValue {
    pub(crate) m: [[f32; 4]; 4],
}

impl MatrixValue {
    /// `Matrix::identity` (`matrix.cpp:118-123`).
    pub(super) fn identity() -> Self {
        let mut value = Self { m: [[0.0; 4]; 4] };
        for (index, row) in value.m.iter_mut().enumerate() {
            row[index] = 1.0;
        }
        value
    }

    /// `Matrix::Matrix()` (`matrix.ipp:19-22`): all zero.
    pub(crate) fn zero() -> Self {
        Self { m: [[0.0; 4]; 4] }
    }

    /// `Matrix::setZero` (`matrix.ipp:45-63`).
    fn set_zero(&mut self) {
        self.m = [[0.0; 4]; 4];
    }

    /// `Matrix::setIdentity` (`matrix.ipp:625-631`): a copy of `identity`.
    fn set_identity(&mut self) {
        *self = Self::identity();
    }

    /// `Matrix::setScale(x, y, z)` (`matrix.cpp:281-306`).
    pub(super) fn set_scale(&mut self, x: f32, y: f32, z: f32) {
        self.m[0] = [x, 0.0, 0.0, 0.0];
        self.m[1] = [0.0, y, 0.0, 0.0];
        self.m[2] = [0.0, 0.0, z, 0.0];
        self.m[3] = [0.0, 0.0, 0.0, 1.0];
    }

    /// `Matrix::setTranslate(x, y, z)` (`matrix.cpp:318-343`).
    fn set_translate(&mut self, x: f32, y: f32, z: f32) {
        self.m[0] = [1.0, 0.0, 0.0, 0.0];
        self.m[1] = [0.0, 1.0, 0.0, 0.0];
        self.m[2] = [0.0, 0.0, 1.0, 0.0];
        self.m[3] = [x, y, z, 1.0];
    }

    /// `Matrix::setRotateX` (`matrix.cpp:434-463`): the trigonometry runs
    /// through doubles and truncates to float.
    pub(super) fn set_rotate_x(&mut self, angle: f32) {
        let sa = (angle as f64).sin() as f32;
        let ca = (angle as f64).cos() as f32;
        self.m[0] = [1.0, 0.0, 0.0, 0.0];
        self.m[1] = [0.0, ca, sa, 0.0];
        self.m[2] = [0.0, -sa, ca, 0.0];
        self.m[3] = [0.0, 0.0, 0.0, 1.0];
    }

    /// `Matrix::setRotateY` (`matrix.cpp:470-499`).
    pub(super) fn set_rotate_y(&mut self, angle: f32) {
        let sa = (angle as f64).sin() as f32;
        let ca = (angle as f64).cos() as f32;
        self.m[0] = [ca, 0.0, -sa, 0.0];
        self.m[1] = [0.0, 1.0, 0.0, 0.0];
        self.m[2] = [sa, 0.0, ca, 0.0];
        self.m[3] = [0.0, 0.0, 0.0, 1.0];
    }

    /// `Matrix::setRotateZ` (`matrix.cpp:506-535`).
    pub(super) fn set_rotate_z(&mut self, angle: f32) {
        let sa = (angle as f64).sin() as f32;
        let ca = (angle as f64).cos() as f32;
        self.m[0] = [ca, sa, 0.0, 0.0];
        self.m[1] = [-sa, ca, 0.0, 0.0];
        self.m[2] = [0.0, 0.0, 1.0, 0.0];
        self.m[3] = [0.0, 0.0, 0.0, 1.0];
    }

    /// `Matrix::setRotate(yaw, pitch, roll)` (`matrix.cpp:586-593`): the
    /// `#if 1` branch -- yaw around Y, then pitch pre-multiplied, then
    /// roll pre-multiplied.
    fn set_rotate_ypr(&mut self, yaw: f32, pitch: f32, roll: f32) {
        self.set_rotate_y(yaw);
        self.pre_rotate_x(pitch);
        self.pre_rotate_z(roll);
    }

    /// `Matrix::preRotateX` (`matrix.cpp:830-847`): rows 1 and 2 mix,
    /// reading row 1 into a temporary first.
    fn pre_rotate_x(&mut self, angle: f32) {
        let sa = (angle as f64).sin() as f32;
        let ca = (angle as f64).cos() as f32;
        let v1 = self.m[1];
        for column in 0..3 {
            self.m[1][column] = ca * v1[column] + sa * self.m[2][column];
            self.m[2][column] = -sa * v1[column] + ca * self.m[2][column];
        }
    }

    /// `Matrix::preRotateZ` (`matrix.cpp:875-892`): rows 0 and 1 mix.
    fn pre_rotate_z(&mut self, angle: f32) {
        let sa = (angle as f64).sin() as f32;
        let ca = (angle as f64).cos() as f32;
        let v0 = self.m[0];
        for column in 0..3 {
            self.m[0][column] = ca * v0[column] + sa * self.m[1][column];
            self.m[1][column] = -sa * v0[column] + ca * self.m[1][column];
        }
    }

    /// `Matrix::multiply` (`matrix.cpp:346-381`): the plain row-major
    /// product of two whole matrices, evaluated cell by cell from the two
    /// unmodified sources.
    pub(super) fn multiply(&mut self, m1: &Self, m2: &Self) {
        let mut out = [[0.0_f32; 4]; 4];
        for row in 0..4 {
            for column in 0..4 {
                out[row][column] = m1.m[row][0] * m2.m[0][column]
                    + m1.m[row][1] * m2.m[1][column]
                    + m1.m[row][2] * m2.m[2][column]
                    + m1.m[row][3] * m2.m[3][column];
            }
        }
        self.m = out;
    }

    /// `Matrix::preMultiply` (`matrix.cpp:391-398`): `this = arg * this`,
    /// through a copy of the old value.
    pub(super) fn pre_multiply(&mut self, arg: &Self) {
        let old = *self;
        self.multiply(arg, &old);
    }

    /// `Matrix::postMultiply` (`matrix.cpp:406-413`): `this = this * arg`.
    fn post_multiply(&mut self, arg: &Self) {
        let old = *self;
        self.multiply(&old, arg);
    }

    /// `Matrix::getDeterminant` (`matrix.ipp:67-88`): the 3x3 part only.
    pub(super) fn get_determinant(&self) -> f32 {
        let m = &self.m;
        m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1])
            - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
            + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0])
    }

    /// `Matrix::invert(matrix)` (`matrix.cpp:614-674`): the 3x4 adjugate.
    /// An exactly zero determinant refuses and leaves this matrix
    /// untouched; the refusal also means columns three of rows 0-2 and
    /// `m[3][3]` stay untouched even on success.
    pub(super) fn invert_from(&mut self, matrix: &Self) -> bool {
        let determinant = matrix.get_determinant();
        if determinant == 0.0 {
            return false;
        }
        let scale = 1.0 / determinant;
        let s = &matrix.m;
        self.m[0][0] = s[1][1] * s[2][2] - s[1][2] * s[2][1];
        self.m[0][1] = s[0][2] * s[2][1] - s[0][1] * s[2][2];
        self.m[0][2] = s[0][1] * s[1][2] - s[0][2] * s[1][1];
        self.m[1][0] = s[1][2] * s[2][0] - s[1][0] * s[2][2];
        self.m[1][1] = s[0][0] * s[2][2] - s[0][2] * s[2][0];
        self.m[1][2] = s[0][2] * s[1][0] - s[0][0] * s[1][2];
        self.m[2][0] = s[1][0] * s[2][1] - s[1][1] * s[2][0];
        self.m[2][1] = s[0][1] * s[2][0] - s[0][0] * s[2][1];
        self.m[2][2] = s[0][0] * s[1][1] - s[0][1] * s[1][0];
        for row in self.m.iter_mut().take(3) {
            for cell in row.iter_mut().take(3) {
                *cell *= scale;
            }
        }
        self.m[3][0] = -(s[3][0] * self.m[0][0] + s[3][1] * self.m[1][0] + s[3][2] * self.m[2][0]);
        self.m[3][1] = -(s[3][0] * self.m[0][1] + s[3][1] * self.m[1][1] + s[3][2] * self.m[2][1]);
        self.m[3][2] = -(s[3][0] * self.m[0][2] + s[3][1] * self.m[1][2] + s[3][2] * self.m[2][2]);
        true
    }

    /// `Matrix::invert()` (`matrix.ipp:533-541`): the copy-then-invert
    /// pair the `PyMatrix` override calls.
    pub(super) fn invert(&mut self) -> bool {
        let old = *self;
        self.invert_from(&old)
    }

    /// `Matrix::isMirrored` (`matrix.cpp:130-136`): the sign of the cross
    /// of the first and third axis rows dotted with the second.
    pub(super) fn is_mirrored(&self) -> bool {
        let v = cross3(self.axis(0), self.axis(2));
        dot3(v, self.axis(1)) > 0.0
    }

    /// `Matrix::lookAt` (`matrix.cpp:695-724`), transcribed step for step:
    /// the axis vectors land in the *columns*, `m[3][0]` is written twice
    /// (the `0.f` from the first row block, then the dot product -- the
    /// dot wins), and `m[0..3][3]` are never written at all.
    pub(super) fn look_at(&mut self, position: [f32; 3], direction: [f32; 3], up: [f32; 3]) {
        let mut direction = direction;
        let mut right;
        normalise3(&mut direction);
        right = cross3(up, direction);
        normalise3(&mut right);
        let up = cross3(direction, right);

        self.m[0][0] = right[0];
        self.m[1][0] = right[1];
        self.m[2][0] = right[2];
        self.m[3][0] = 0.0;

        self.m[0][1] = up[0];
        self.m[1][1] = up[1];
        self.m[2][1] = up[2];
        self.m[3][1] = 0.0;

        self.m[0][2] = direction[0];
        self.m[1][2] = direction[1];
        self.m[2][2] = direction[2];
        self.m[3][2] = 0.0;

        self.m[3][0] = -dot3(position, right);
        self.m[3][1] = -dot3(position, up);
        self.m[3][2] = -dot3(position, direction);
        self.m[3][3] = 1.0;
    }

    /// `Matrix::orthogonalProjection` (`matrix.ipp:563-577`): the rows of
    /// the left-handed ortho matrix.
    fn orthogonal_projection(&mut self, w: f32, h: f32, zn: f32, zf: f32) {
        self.m[0] = [2.0 / w, 0.0, 0.0, 0.0];
        self.m[1] = [0.0, 2.0 / h, 0.0, 0.0];
        self.m[2] = [0.0, 0.0, 1.0 / (zf - zn), 0.0];
        self.m[3] = [0.0, 0.0, zn / (zn - zf), 1.0];
    }

    /// `Matrix::perspectiveProjection` (`matrix.ipp:579-612`): every cell
    /// assigned, the cotangent through a double `tan`.
    fn perspective_projection(&mut self, fov: f32, aspect_ratio: f32, near: f32, far: f32) {
        let cot = 1.0 / ((fov * 0.5) as f64).tan() as f32;
        let scale = 1.0 / (far - near);
        self.m[0] = [cot / aspect_ratio, 0.0, 0.0, 0.0];
        self.m[1] = [0.0, cot, 0.0, 0.0];
        self.m[2] = [0.0, 0.0, scale * far, 1.0];
        self.m[3] = [0.0, 0.0, -(scale * far * near), 0.0];
    }

    /// `Matrix::translation(v)` (`matrix.ipp:615-621`): the bottom row
    /// becomes the vector, then `m[3][3] = 1`.
    pub(super) fn set_translation(&mut self, v: [f32; 3]) {
        self.m[3] = [v[0], v[1], v[2], 1.0];
    }

    /// One axis vector: the first three floats of row `axis`
    /// (`applyToUnitAxisVector`, `matrix.ipp:368-376`).
    pub(super) fn axis(&self, axis: usize) -> [f32; 3] {
        [self.m[axis][0], self.m[axis][1], self.m[axis][2]]
    }

    /// `Matrix::applyToOrigin` (`matrix.ipp:379-386`): `(*this)[3]`.
    pub(super) fn origin(&self) -> [f32; 3] {
        self.axis(3)
    }

    /// `Matrix::applyPoint(Vector3&)` (`matrix.ipp:159-165` with the
    /// Vector4 body at `:191-222`): the point transform, translation
    /// included; the Vector4 result's first three components come back.
    pub(super) fn apply_point(&self, v: [f32; 3]) -> [f32; 3] {
        let m = &self.m;
        let mut out = [0.0_f32; 4];
        for (index, cell) in out.iter_mut().enumerate() {
            *cell = v[0] * m[0][index] + v[1] * m[1][index] + v[2] * m[2][index] + m[3][index];
        }
        [out[0], out[1], out[2]]
    }

    /// `Matrix::applyPoint(Vector4&, Vector4&)` (`matrix.ipp:234-276`).
    pub(super) fn apply_point_v4(&self, v: [f32; 4]) -> [f32; 4] {
        let m = &self.m;
        let mut out = [0.0_f32; 4];
        for (index, cell) in out.iter_mut().enumerate() {
            *cell =
                v[0] * m[0][index] + v[1] * m[1][index] + v[2] * m[2][index] + v[3] * m[3][index];
        }
        out
    }

    /// `Matrix::applyVector` (`matrix.ipp:279-300`): the vector transform,
    /// no translation.
    pub(super) fn apply_vector(&self, v: [f32; 3]) -> [f32; 3] {
        let m = &self.m;
        [
            v[0] * m[0][0] + v[1] * m[1][0] + v[2] * m[2][0],
            v[0] * m[0][1] + v[1] * m[1][1] + v[2] * m[2][1],
            v[0] * m[0][2] + v[1] * m[1][2] + v[2] * m[2][2],
        ]
    }

    /// `Matrix::yaw` (`matrix.ipp:638-651`): the normalised z axis
    /// through `atan2f(x, z)`.
    pub(super) fn yaw(&self) -> f32 {
        let mut zdir = self.axis(2);
        normalise3(&mut zdir);
        zdir[0].atan2(zdir[2])
    }

    /// `Matrix::pitch` (`matrix.ipp:657-670`): `-\asin(clamp(zdir.y))`.
    pub(super) fn pitch(&self) -> f32 {
        let mut zdir = self.axis(2);
        normalise3(&mut zdir);
        -zdir[1].clamp(-1.0, 1.0).asin()
    }

    /// `Matrix::roll` (`matrix.ipp:672-691`): both leading rows
    /// normalised, `atan2f(xdir.y, ydir.y)`.
    pub(super) fn roll(&self) -> f32 {
        let mut xdir = self.axis(0);
        let mut ydir = self.axis(1);
        normalise3(&mut xdir);
        normalise3(&mut ydir);
        xdir[1].atan2(ydir[1])
    }

    /// The raw little-endian bytes of the block, the `__getstate__` blob
    /// (`script_math.cpp:744-758`).
    fn state_bytes(&self) -> [u8; 64] {
        let mut blob = [0_u8; 64];
        for (index, cell) in self.m.iter().flatten().enumerate() {
            blob[index * 4..index * 4 + 4].copy_from_slice(&cell.to_le_bytes());
        }
        blob
    }

    /// Rebuild from the raw bytes, the `__setstate__` body
    /// (`script_math.cpp:778`).
    fn from_state_bytes(bytes: &[u8]) -> Self {
        let mut value = Self { m: [[0.0; 4]; 4] };
        for (index, cell) in value.m.iter_mut().flatten().enumerate() {
            *cell = f32::from_le_bytes([
                bytes[index * 4],
                bytes[index * 4 + 1],
                bytes[index * 4 + 2],
                bytes[index * 4 + 3],
            ]);
        }
        value
    }
}

/// The `Math.Matrix` instance body: the value block. `PyMatrix` carries no
/// other state -- `autoTick` is false (`script_math.hpp:162`), so nothing
/// registers with the provider store.
struct MatrixBody {
    value: MatrixValue,
}

/// The `MatrixProvider` virtual `matrix()` for any provider object: a
/// `Math.Matrix` copies its body; the provider types answer through
/// [`provider::matrix_of`]. `None` for a non-provider object.
///
/// # Safety
///
/// `object` must be a borrowed object the interpreter handed out, or null.
pub(crate) unsafe fn matrix_of(object: *mut capi::PyObject) -> Option<MatrixValue> {
    if unsafe { check_type(object, MATRIX_NAME) } {
        // SAFETY: the check above guarantees the instance shape.
        return Some(unsafe { (*body::<MatrixBody>(object)).value });
    }
    unsafe { provider::matrix_of(object) }
}

/// A fresh `Math.Matrix` instance the caller owns: the `new PyMatrix`
/// the `getData(const Matrix&)` path answers with (`script.cpp:2400`).
/// Used by the `ResMgr` faces (`asMatrix`, `readMatrix` build real
/// matrix objects).
///
/// # Safety
///
/// The interpreter must be up and the `Math` module installed.
pub(crate) unsafe fn matrix_object(value: MatrixValue) -> PyRef {
    let address = type_address(MATRIX_NAME).expect("the matrix type is installed");
    let type_object = PyRef::borrowed(address as *mut capi::PyObject);
    // SAFETY: the type is one of this module's, and the interpreter is up.
    let instance = unsafe { binding::instantiate(&type_object) }
        .expect("the matrix instance allocation cannot fail");
    // SAFETY: the instance is the fresh allocation this call owns.
    unsafe {
        (*body::<MatrixBody>(instance.as_ptr())).value = value;
    }
    instance
}

/// One `MatrixProviderPtr` converter step
/// (`script.hpp:592-620`): `None` is the null pointer, a provider passes
/// through, anything else raises `"<varName> must be set to a
/// MatrixProvider or None"`. The `Option` layer mirrors the nullable
/// pointer: `None` means the argument was `None`.
///
/// # Safety
///
/// `object` must be a borrowed object the interpreter handed out, or null.
pub(super) unsafe fn coerce_matrix_provider(
    object: *mut capi::PyObject,
    var_name: &str,
) -> Option<Option<*mut capi::PyObject>> {
    if std::ptr::eq(object, capi::py_none()) {
        return Some(None);
    }
    if unsafe { provider::is_matrix_provider(object) } {
        return Some(Some(object));
    }
    raise_type_error(&format!(
        "{var_name} must be set to a MatrixProvider or None"
    ));
    None
}

/// The one-argument provider parse shared by `set`, `preMultiply` and
/// `postMultiply`: the count check first, then the converter with the
/// `() argument 1` name, then the `NZARG` wall -- a `None` argument is the
/// reference's `"() argument 1 cannot be None"`
/// (`script.hpp:1142-1151`).
///
/// # Safety
///
/// `args` must be the argument tuple a C call handed over.
unsafe fn parse_provider_argument(
    name: &str,
    args: *mut capi::PyObject,
) -> Option<*mut capi::PyObject> {
    if unsafe { capi::PyTuple_Size(args) } != 1 {
        arg_count_error(name, &["MatrixProvider"]);
        return None;
    }
    // SAFETY: the size check above bounds the index.
    let item = unsafe { capi::PyTuple_GetItem(args, 0) };
    let coerced = unsafe { coerce_matrix_provider(item, "() argument 1") }?;
    match coerced {
        Some(object) => Some(object),
        None => {
            raise_type_error("() argument 1 cannot be None");
            None
        }
    }
}

/// The `() argument N` name of one argument.
fn argument_name(index: usize) -> String {
    format!("() argument {index}")
}

/// The `PyMatrix` constructor (`script_math.cpp:780-798`): `|O` -- zero
/// arguments build the identity, one provider argument is snapshotted,
/// anything else (a second argument, a non-provider, `None`) raises the
/// one sentence. Keyword arguments are accepted and ignored, the way
/// `PyArg_ParseTuple` leaves them.
///
/// # Safety
///
/// A C `tp_new` call: the interpreter holds the GIL.
unsafe extern "C" fn matrix_new(
    type_object: *mut capi::PyTypeObject,
    args: *mut capi::PyObject,
    _kwargs: *mut capi::PyObject,
) -> *mut capi::PyObject {
    let handle = PyRef::borrowed(type_object.cast());
    // SAFETY: the type is one of this module's and the interpreter is up.
    let Some(instance) = (unsafe { binding::instantiate(&handle) }) else {
        raise_type_error("Matrix allocation failed");
        return std::ptr::null_mut();
    };
    // The `|O` parse accepts exactly one argument; more fail the parse
    // and the constructor replaces whatever error is pending with its
    // one sentence, then returns the null object.
    let argument = if unsafe { capi::PyTuple_Size(args) } > 1 {
        raise_type_error("Math.Matrix(): Expected an optional MatrixProvider");
        return std::ptr::null_mut();
    } else if unsafe { capi::PyTuple_Size(args) } == 0 {
        // `PyTuple_GetItem` on an empty tuple answers null *and* leaves
        // an `IndexError` behind, which a CPython constructor must never
        // return with, so the absent argument is read only when present.
        std::ptr::null_mut()
    } else {
        // SAFETY: the count check above bounds the index.
        unsafe { capi::PyTuple_GetItem(args, 0) }
    };
    if !argument.is_null() && !unsafe { provider::is_matrix_provider(argument) } {
        raise_type_error("Math.Matrix(): Expected an optional MatrixProvider");
        return std::ptr::null_mut();
    }
    let initial = if argument.is_null() {
        MatrixValue::identity()
    } else {
        // SAFETY: the check above guarantees a matrix provider, which
        // always answers its matrix.
        unsafe { matrix_of(argument) }.expect("a provider answers its matrix")
    };
    // SAFETY: the instance is the fresh copy this call built.
    unsafe {
        (*body::<MatrixBody>(instance.as_ptr())).value = initial;
    }
    own_raw(instance)
}

/// The `PyMatrix` call slot (`script_math.hpp:241-242`): `_pyCall` is
/// `_py_get`, which the el7 build compiles as the `get` machinery --
/// `argCountError("get", 2, 2, "uint", "uint")`, the two arguments through
/// `setData(.., uint, "() argument N")`, then `m[col&3][row&3]` as a
/// float (disassembly of `BW::PyMatrix::_pyCall` in
/// `build/el7/lib/libpyscript.a`).
///
/// # Safety
///
/// A C `tp_call` call: the interpreter holds the GIL.
unsafe extern "C" fn matrix_call(
    self_: *mut capi::PyObject,
    args: *mut capi::PyObject,
    _kwargs: *mut capi::PyObject,
) -> *mut capi::PyObject {
    if unsafe { capi::PyTuple_Size(args) } != 2 {
        arg_count_error("get", &["uint", "uint"]);
        return std::ptr::null_mut();
    }
    // SAFETY: the size check above bounds both indices.
    let (first, second) = (unsafe { capi::PyTuple_GetItem(args, 0) }, unsafe {
        capi::PyTuple_GetItem(args, 1)
    });
    let Some(column) = (unsafe { coerce_uint(first, &argument_name(1)) }) else {
        return std::ptr::null_mut();
    };
    let Some(row) = (unsafe { coerce_uint(second, &argument_name(2)) }) else {
        return std::ptr::null_mut();
    };
    // SAFETY: the caller guarantees the instance shape.
    let value = unsafe { (*body::<MatrixBody>(self_)).value };
    float_object(value.m[(column & 3) as usize][(row & 3) as usize] as f64)
}

/// The shared one-`Vector3`-argument parse: the count check, then
/// `setData` with the `() argument 1` name.
///
/// # Safety
///
/// `args` must be the argument tuple a C call handed over.
unsafe fn parse_vector3_argument(name: &str, args: *mut capi::PyObject) -> Option<[f32; 3]> {
    if unsafe { capi::PyTuple_Size(args) } != 1 {
        arg_count_error(name, &["Vector3"]);
        return None;
    }
    // SAFETY: the size check above bounds the index.
    let item = unsafe { capi::PyTuple_GetItem(args, 0) };
    unsafe { parse_vector_like(item, 3, &argument_name(1)) }.map(|v| [v[0], v[1], v[2]])
}

/// The shared one-float-angle parse: the count check, then the float
/// parse, then the `|a| <= 100` window (`script_math.hpp:66-75`).
///
/// # Safety
///
/// `args` must be the argument tuple a C call handed over.
unsafe fn parse_angle_argument(name: &str, args: *mut capi::PyObject) -> Option<f32> {
    if unsafe { capi::PyTuple_Size(args) } != 1 {
        arg_count_error(name, &["float"]);
        return None;
    }
    // SAFETY: the size check above bounds the index.
    let item = unsafe { capi::PyTuple_GetItem(args, 0) };
    let angle = unsafe { coerce_float(item, &argument_name(1)) }? as f32;
    if !is_valid_angle(angle) {
        raise_type_error("() argument 1 must be a valid angle");
        return None;
    }
    Some(angle)
}

/// # Safety
///
/// A C method call; the interpreter holds the GIL.
unsafe extern "C" fn method_set(
    self_: *mut capi::PyObject,
    args: *mut capi::PyObject,
) -> *mut capi::PyObject {
    let Some(object) = (unsafe { parse_provider_argument("set", args) }) else {
        return std::ptr::null_mut();
    };
    // SAFETY: the parse above guarantees a provider object.
    let snapshot = unsafe { matrix_of(object) }.expect("a provider answers its matrix");
    // SAFETY: the caller guarantees the instance shape.
    unsafe { (*body::<MatrixBody>(self_)).value = snapshot };
    capi::py_none()
}

/// # Safety
///
/// A C method call; the interpreter holds the GIL.
unsafe extern "C" fn method_set_zero(
    self_: *mut capi::PyObject,
    args: *mut capi::PyObject,
) -> *mut capi::PyObject {
    if unsafe { capi::PyTuple_Size(args) } != 0 {
        takes_no_arguments("setZero");
        return std::ptr::null_mut();
    }
    // SAFETY: the caller guarantees the instance shape.
    unsafe { (*body::<MatrixBody>(self_)).value.set_zero() };
    capi::py_none()
}

/// # Safety
///
/// A C method call; the interpreter holds the GIL.
unsafe extern "C" fn method_set_identity(
    self_: *mut capi::PyObject,
    args: *mut capi::PyObject,
) -> *mut capi::PyObject {
    if unsafe { capi::PyTuple_Size(args) } != 0 {
        takes_no_arguments("setIdentity");
        return std::ptr::null_mut();
    }
    // SAFETY: the caller guarantees the instance shape.
    unsafe { (*body::<MatrixBody>(self_)).value.set_identity() };
    capi::py_none()
}

/// # Safety
///
/// A C method call; the interpreter holds the GIL.
unsafe extern "C" fn method_set_scale(
    self_: *mut capi::PyObject,
    args: *mut capi::PyObject,
) -> *mut capi::PyObject {
    let Some(scale) = (unsafe { parse_vector3_argument("setScale", args) }) else {
        return std::ptr::null_mut();
    };
    // SAFETY: the caller guarantees the instance shape.
    unsafe {
        (*body::<MatrixBody>(self_))
            .value
            .set_scale(scale[0], scale[1], scale[2])
    };
    capi::py_none()
}

/// # Safety
///
/// A C method call; the interpreter holds the GIL.
unsafe extern "C" fn method_set_translate(
    self_: *mut capi::PyObject,
    args: *mut capi::PyObject,
) -> *mut capi::PyObject {
    let Some(position) = (unsafe { parse_vector3_argument("setTranslate", args) }) else {
        return std::ptr::null_mut();
    };
    // SAFETY: the caller guarantees the instance shape.
    unsafe {
        (*body::<MatrixBody>(self_))
            .value
            .set_translate(position[0], position[1], position[2])
    };
    capi::py_none()
}

/// # Safety
///
/// A C method call; the interpreter holds the GIL.
unsafe extern "C" fn method_set_rotate_x(
    self_: *mut capi::PyObject,
    args: *mut capi::PyObject,
) -> *mut capi::PyObject {
    let Some(angle) = (unsafe { parse_angle_argument("setRotateX", args) }) else {
        return std::ptr::null_mut();
    };
    // SAFETY: the caller guarantees the instance shape.
    unsafe { (*body::<MatrixBody>(self_)).value.set_rotate_x(angle) };
    capi::py_none()
}

/// # Safety
///
/// A C method call; the interpreter holds the GIL.
unsafe extern "C" fn method_set_rotate_y(
    self_: *mut capi::PyObject,
    args: *mut capi::PyObject,
) -> *mut capi::PyObject {
    let Some(angle) = (unsafe { parse_angle_argument("setRotateY", args) }) else {
        return std::ptr::null_mut();
    };
    // SAFETY: the caller guarantees the instance shape.
    unsafe { (*body::<MatrixBody>(self_)).value.set_rotate_y(angle) };
    capi::py_none()
}

/// # Safety
///
/// A C method call; the interpreter holds the GIL.
unsafe extern "C" fn method_set_rotate_z(
    self_: *mut capi::PyObject,
    args: *mut capi::PyObject,
) -> *mut capi::PyObject {
    let Some(angle) = (unsafe { parse_angle_argument("setRotateZ", args) }) else {
        return std::ptr::null_mut();
    };
    // SAFETY: the caller guarantees the instance shape.
    unsafe { (*body::<MatrixBody>(self_)).value.set_rotate_z(angle) };
    capi::py_none()
}

/// The `setRotateYPR` parse is the vector3 form with the per-element
/// angle window after it (`script_math.hpp:80-89`): element checks run in
/// order, element zero first, and the first failure names that element.
///
/// # Safety
///
/// A C method call; the interpreter holds the GIL.
unsafe extern "C" fn method_set_rotate_ypr(
    self_: *mut capi::PyObject,
    args: *mut capi::PyObject,
) -> *mut capi::PyObject {
    if unsafe { capi::PyTuple_Size(args) } != 1 {
        arg_count_error("setRotateYPR", &["Vector3"]);
        return std::ptr::null_mut();
    }
    // SAFETY: the size check above bounds the index.
    let item = unsafe { capi::PyTuple_GetItem(args, 0) };
    let Some(ypr) = (unsafe { parse_vector_like(item, 3, &argument_name(1)) }) else {
        return std::ptr::null_mut();
    };
    for (element, angle) in ypr.iter().enumerate() {
        if !is_valid_angle(*angle) {
            raise_type_error(&format!(
                "() argument 1 element {element} must be a valid angle"
            ));
            return std::ptr::null_mut();
        }
    }
    // SAFETY: the caller guarantees the instance shape.
    unsafe {
        (*body::<MatrixBody>(self_))
            .value
            .set_rotate_ypr(ypr[0], ypr[1], ypr[2])
    };
    capi::py_none()
}

/// # Safety
///
/// A C method call; the interpreter holds the GIL.
unsafe extern "C" fn method_pre_multiply(
    self_: *mut capi::PyObject,
    args: *mut capi::PyObject,
) -> *mut capi::PyObject {
    let Some(object) = (unsafe { parse_provider_argument("preMultiply", args) }) else {
        return std::ptr::null_mut();
    };
    // SAFETY: the parse above guarantees a provider object.
    let snapshot = unsafe { matrix_of(object) }.expect("a provider answers its matrix");
    // SAFETY: the caller guarantees the instance shape.
    unsafe { (*body::<MatrixBody>(self_)).value.pre_multiply(&snapshot) };
    capi::py_none()
}

/// # Safety
///
/// A C method call; the interpreter holds the GIL.
unsafe extern "C" fn method_post_multiply(
    self_: *mut capi::PyObject,
    args: *mut capi::PyObject,
) -> *mut capi::PyObject {
    let Some(object) = (unsafe { parse_provider_argument("postMultiply", args) }) else {
        return std::ptr::null_mut();
    };
    // SAFETY: the parse above guarantees a provider object.
    let snapshot = unsafe { matrix_of(object) }.expect("a provider answers its matrix");
    // SAFETY: the caller guarantees the instance shape.
    unsafe { (*body::<MatrixBody>(self_)).value.post_multiply(&snapshot) };
    capi::py_none()
}

/// The `invert` override (`script_math.hpp:168-176`): the base inversion,
/// then the `ValueError` on refusal. `RETOK` answers `None` on success.
///
/// # Safety
///
/// A C method call; the interpreter holds the GIL.
unsafe extern "C" fn method_invert(
    self_: *mut capi::PyObject,
    args: *mut capi::PyObject,
) -> *mut capi::PyObject {
    if unsafe { capi::PyTuple_Size(args) } != 0 {
        takes_no_arguments("invert");
        return std::ptr::null_mut();
    }
    // SAFETY: the caller guarantees the instance shape.
    if unsafe { (*body::<MatrixBody>(self_)).value.invert() } {
        capi::py_none()
    } else {
        super::raise_value_error("Matrix could not be inverted");
        std::ptr::null_mut()
    }
}

/// # Safety
///
/// A C method call; the interpreter holds the GIL.
unsafe extern "C" fn method_look_at(
    self_: *mut capi::PyObject,
    args: *mut capi::PyObject,
) -> *mut capi::PyObject {
    if unsafe { capi::PyTuple_Size(args) } != 3 {
        arg_count_error("lookAt", &["Vector3", "Vector3", "Vector3"]);
        return std::ptr::null_mut();
    }
    let mut parsed = [[0.0_f32; 3]; 3];
    for (index, point) in parsed.iter_mut().enumerate() {
        // SAFETY: the count check above bounds the index.
        let item = unsafe { capi::PyTuple_GetItem(args, index as isize) };
        let Some(value) = (unsafe { parse_vector_like(item, 3, &argument_name(index + 1)) }) else {
            return std::ptr::null_mut();
        };
        *point = [value[0], value[1], value[2]];
    }
    // SAFETY: the caller guarantees the instance shape.
    unsafe {
        (*body::<MatrixBody>(self_))
            .value
            .look_at(parsed[0], parsed[1], parsed[2])
    };
    capi::py_none()
}

/// # Safety
///
/// A C method call; the interpreter holds the GIL.
unsafe extern "C" fn method_set_element(
    self_: *mut capi::PyObject,
    args: *mut capi::PyObject,
) -> *mut capi::PyObject {
    if unsafe { capi::PyTuple_Size(args) } != 3 {
        arg_count_error("setElement", &["uint", "uint", "float"]);
        return std::ptr::null_mut();
    }
    let mut parsed = [0.0_f64; 3];
    for (index, slot) in parsed.iter_mut().enumerate() {
        // SAFETY: the count check above bounds the index.
        let item = unsafe { capi::PyTuple_GetItem(args, index as isize) };
        let value = match index {
            0 | 1 => unsafe { coerce_uint(item, &argument_name(index + 1)) }.map(f64::from),
            _ => unsafe { coerce_float(item, &argument_name(index + 1)) },
        };
        let Some(value) = value else {
            return std::ptr::null_mut();
        };
        *slot = value;
    }
    // SAFETY: the caller guarantees the instance shape.
    unsafe {
        (*body::<MatrixBody>(self_)).value.m[(parsed[0] as u32 & 3) as usize]
            [(parsed[1] as u32 & 3) as usize] = parsed[2] as f32;
    }
    capi::py_none()
}

/// # Safety
///
/// A C method call; the interpreter holds the GIL.
unsafe extern "C" fn method_get(
    self_: *mut capi::PyObject,
    args: *mut capi::PyObject,
) -> *mut capi::PyObject {
    if unsafe { capi::PyTuple_Size(args) } != 2 {
        arg_count_error("get", &["uint", "uint"]);
        return std::ptr::null_mut();
    }
    // SAFETY: the size check above bounds both indices.
    let (first, second) = (unsafe { capi::PyTuple_GetItem(args, 0) }, unsafe {
        capi::PyTuple_GetItem(args, 1)
    });
    let Some(column) = (unsafe { coerce_uint(first, &argument_name(1)) }) else {
        return std::ptr::null_mut();
    };
    let Some(row) = (unsafe { coerce_uint(second, &argument_name(2)) }) else {
        return std::ptr::null_mut();
    };
    // SAFETY: the caller guarantees the instance shape.
    let value = unsafe { (*body::<MatrixBody>(self_)).value };
    float_object(value.m[(column & 3) as usize][(row & 3) as usize] as f64)
}

/// # Safety
///
/// A C method call; the interpreter holds the GIL.
unsafe extern "C" fn method_apply_point(
    self_: *mut capi::PyObject,
    args: *mut capi::PyObject,
) -> *mut capi::PyObject {
    let Some(point) = (unsafe { parse_vector3_argument("applyPoint", args) }) else {
        return std::ptr::null_mut();
    };
    // SAFETY: the caller guarantees the instance shape and the vector
    // types are installed.
    let value = unsafe { (*body::<MatrixBody>(self_)).value }.apply_point(point);
    own_raw(unsafe { vector_object([value[0], value[1], value[2], 0.0], 3) })
}

/// # Safety
///
/// A C method call; the interpreter holds the GIL.
unsafe extern "C" fn method_apply_v4_point(
    self_: *mut capi::PyObject,
    args: *mut capi::PyObject,
) -> *mut capi::PyObject {
    if unsafe { capi::PyTuple_Size(args) } != 1 {
        arg_count_error("applyV4Point", &["Vector4"]);
        return std::ptr::null_mut();
    }
    // SAFETY: the size check above bounds the index.
    let item = unsafe { capi::PyTuple_GetItem(args, 0) };
    let Some(point) = (unsafe { parse_vector_like(item, 4, &argument_name(1)) }) else {
        return std::ptr::null_mut();
    };
    // SAFETY: the caller guarantees the instance shape and the vector
    // types are installed.
    let value = unsafe { (*body::<MatrixBody>(self_)).value }.apply_point_v4(point);
    own_raw(unsafe { vector_object(value, 4) })
}

/// # Safety
///
/// A C method call; the interpreter holds the GIL.
unsafe extern "C" fn method_apply_vector(
    self_: *mut capi::PyObject,
    args: *mut capi::PyObject,
) -> *mut capi::PyObject {
    let Some(vector) = (unsafe { parse_vector3_argument("applyVector", args) }) else {
        return std::ptr::null_mut();
    };
    // SAFETY: the caller guarantees the instance shape and the vector
    // types are installed.
    let value = unsafe { (*body::<MatrixBody>(self_)).value }.apply_vector(vector);
    own_raw(unsafe { vector_object([value[0], value[1], value[2], 0.0], 3) })
}

/// # Safety
///
/// A C method call; the interpreter holds the GIL.
unsafe extern "C" fn method_apply_to_axis(
    self_: *mut capi::PyObject,
    args: *mut capi::PyObject,
) -> *mut capi::PyObject {
    if unsafe { capi::PyTuple_Size(args) } != 1 {
        arg_count_error("applyToAxis", &["uint"]);
        return std::ptr::null_mut();
    }
    // SAFETY: the size check above bounds the index.
    let item = unsafe { capi::PyTuple_GetItem(args, 0) };
    let Some(axis) = (unsafe { coerce_uint(item, &argument_name(1)) }) else {
        return std::ptr::null_mut();
    };
    // SAFETY: the caller guarantees the instance shape and the vector
    // types are installed.
    let value = unsafe { (*body::<MatrixBody>(self_)).value }.axis((axis & 3) as usize);
    own_raw(unsafe { vector_object([value[0], value[1], value[2], 0.0], 3) })
}

/// # Safety
///
/// A C method call; the interpreter holds the GIL.
unsafe extern "C" fn method_apply_to_origin(
    self_: *mut capi::PyObject,
    args: *mut capi::PyObject,
) -> *mut capi::PyObject {
    if unsafe { capi::PyTuple_Size(args) } != 0 {
        takes_no_arguments("applyToOrigin");
        return std::ptr::null_mut();
    }
    // SAFETY: the caller guarantees the instance shape and the vector
    // types are installed.
    let value = unsafe { (*body::<MatrixBody>(self_)).value }.origin();
    own_raw(unsafe { vector_object([value[0], value[1], value[2], 0.0], 3) })
}

/// # Safety
///
/// A C method call; the interpreter holds the GIL.
unsafe extern "C" fn method_orthogonal_projection(
    self_: *mut capi::PyObject,
    args: *mut capi::PyObject,
) -> *mut capi::PyObject {
    let Some((w, h, zn, zf)) = (unsafe { parse_four_floats("orthogonalProjection", args) }) else {
        return std::ptr::null_mut();
    };
    // SAFETY: the caller guarantees the instance shape.
    unsafe {
        (*body::<MatrixBody>(self_))
            .value
            .orthogonal_projection(w, h, zn, zf)
    };
    capi::py_none()
}

/// # Safety
///
/// A C method call; the interpreter holds the GIL.
unsafe extern "C" fn method_perspective_projection(
    self_: *mut capi::PyObject,
    args: *mut capi::PyObject,
) -> *mut capi::PyObject {
    let Some((fov, aspect, near, far)) =
        (unsafe { parse_four_floats("perspectiveProjection", args) })
    else {
        return std::ptr::null_mut();
    };
    // SAFETY: the caller guarantees the instance shape.
    unsafe {
        (*body::<MatrixBody>(self_))
            .value
            .perspective_projection(fov, aspect, near, far)
    };
    capi::py_none()
}

/// The four-float parse the two projection methods share: the count check
/// names the method, then the arguments parse left to right.
///
/// # Safety
///
/// `args` must be the argument tuple a C call handed over.
unsafe fn parse_four_floats(name: &str, args: *mut capi::PyObject) -> Option<(f32, f32, f32, f32)> {
    if unsafe { capi::PyTuple_Size(args) } != 4 {
        arg_count_error(name, &["float", "float", "float", "float"]);
        return None;
    }
    let mut parsed = [0.0_f32; 4];
    for (index, value) in parsed.iter_mut().enumerate() {
        // SAFETY: the count check above bounds the index.
        let item = unsafe { capi::PyTuple_GetItem(args, index as isize) };
        let raw = unsafe { coerce_float(item, &argument_name(index + 1)) }?;
        *value = raw as f32;
    }
    Some((parsed[0], parsed[1], parsed[2], parsed[3]))
}

/// `__getstate__` (`script_math.cpp:744-758`): the 64 raw bytes of the
/// block. The reference hands the argument tuple straight to
/// `PyBytes_FromStringAndSize` without looking at it, so any argument
/// list is accepted.
///
/// # Safety
///
/// A C method call; the interpreter holds the GIL.
unsafe extern "C" fn method_get_state(
    self_: *mut capi::PyObject,
    _args: *mut capi::PyObject,
) -> *mut capi::PyObject {
    // SAFETY: the caller guarantees the instance shape.
    let blob = unsafe { (*body::<MatrixBody>(self_)).value }.state_bytes();
    // SAFETY: the interpreter is up.
    unsafe { capi::PyBytes_FromStringAndSize(blob.as_ptr().cast(), 64) }
}

/// `__setstate__` (`script_math.cpp:761-780`): exactly one argument, a
/// bytes object of exactly the block width; each failure carries its own
/// sentence.
///
/// # Safety
///
/// A C method call; the interpreter holds the GIL.
unsafe extern "C" fn method_set_state(
    self_: *mut capi::PyObject,
    args: *mut capi::PyObject,
) -> *mut capi::PyObject {
    if unsafe { capi::PyTuple_Size(args) } != 1 {
        raise_type_error("Matrix.__setstate__ was given wrong args");
        return std::ptr::null_mut();
    }
    // SAFETY: the size check above bounds the index.
    let item = unsafe { capi::PyTuple_GetItem(args, 0) };
    if !unsafe { capi::py_bytes_check(item) } {
        super::raise_value_error("Matrix.__setstate__ was given bad state");
        return std::ptr::null_mut();
    }
    let mut pointer: *mut std::os::raw::c_char = std::ptr::null_mut();
    let mut length: isize = 0;
    // SAFETY: the check above guarantees a bytes object.
    if unsafe { capi::PyBytes_AsStringAndSize(item, &mut pointer, &mut length) } != 0
        || length != 64
    {
        super::raise_value_error("Matrix.__setstate__ was given bad state");
        return std::ptr::null_mut();
    }
    // SAFETY: the length check above bounds the read.
    let bytes = unsafe { std::slice::from_raw_parts(pointer.cast::<u8>(), 64) };
    let value = MatrixValue::from_state_bytes(bytes);
    // SAFETY: the caller guarantees the instance shape.
    unsafe { (*body::<MatrixBody>(self_)).value = value };
    capi::py_none()
}

/// # Safety
///
/// A C getter call; the interpreter holds the GIL.
unsafe extern "C" fn get_determinant(
    object: *mut capi::PyObject,
    _closure: *mut std::os::raw::c_void,
) -> *mut capi::PyObject {
    // SAFETY: the caller guarantees the instance shape.
    let value = unsafe { (*body::<MatrixBody>(object)).value };
    float_object(value.get_determinant() as f64)
}

/// # Safety
///
/// A C getter call; the interpreter holds the GIL.
unsafe extern "C" fn get_is_mirrored(
    object: *mut capi::PyObject,
    _closure: *mut std::os::raw::c_void,
) -> *mut capi::PyObject {
    // SAFETY: the caller guarantees the instance shape.
    let value = unsafe { (*body::<MatrixBody>(object)).value };
    bool_object(value.is_mirrored())
}

/// # Safety
///
/// A C getter call; the interpreter holds the GIL.
unsafe extern "C" fn get_yaw(
    object: *mut capi::PyObject,
    _closure: *mut std::os::raw::c_void,
) -> *mut capi::PyObject {
    // SAFETY: the caller guarantees the instance shape.
    float_object(unsafe { (*body::<MatrixBody>(object)).value }.yaw() as f64)
}

/// # Safety
///
/// A C getter call; the interpreter holds the GIL.
unsafe extern "C" fn get_pitch(
    object: *mut capi::PyObject,
    _closure: *mut std::os::raw::c_void,
) -> *mut capi::PyObject {
    // SAFETY: the caller guarantees the instance shape.
    float_object(unsafe { (*body::<MatrixBody>(object)).value }.pitch() as f64)
}

/// # Safety
///
/// A C getter call; the interpreter holds the GIL.
unsafe extern "C" fn get_roll(
    object: *mut capi::PyObject,
    _closure: *mut std::os::raw::c_void,
) -> *mut capi::PyObject {
    // SAFETY: the caller guarantees the instance shape.
    float_object(unsafe { (*body::<MatrixBody>(object)).value }.roll() as f64)
}

/// The `translation` getter: `applyToOrigin` through the `getData(Vector3)`
/// copy (`script_math.hpp:226-228`).
///
/// # Safety
///
/// A C getter call; the interpreter holds the GIL and the vector types are
/// installed.
unsafe extern "C" fn get_translation(
    object: *mut capi::PyObject,
    _closure: *mut std::os::raw::c_void,
) -> *mut capi::PyObject {
    // SAFETY: the caller guarantees the instance shape.
    let value = unsafe { (*body::<MatrixBody>(object)).value }.origin();
    own_raw(unsafe { vector_object([value[0], value[1], value[2], 0.0], 3) })
}

/// The `translation` setter: the `PY_WRITABLE_ACCESSOR_ATTRIBUTE_SET`
/// shape (`pyobject_plus.hpp:531-540`) -- parse with the attribute name as
/// the var name, call the setter only on a clean parse.
///
/// # Safety
///
/// A C setter call; the interpreter holds the GIL.
unsafe extern "C" fn set_translation(
    object: *mut capi::PyObject,
    value: *mut capi::PyObject,
    _closure: *mut std::os::raw::c_void,
) -> c_int {
    let Some(parsed) = (unsafe { parse_vector_like(value, 3, "translation") }) else {
        return -1;
    };
    // SAFETY: the caller guarantees the instance shape.
    unsafe {
        (*body::<MatrixBody>(object))
            .value
            .set_translation([parsed[0], parsed[1], parsed[2]])
    };
    0
}

/// The full `Math.Matrix` type description: the 21 methods and 7
/// attributes the manifest pins, the call slot, and the pickling pair.
fn matrix_spec() -> TypeSpec {
    TypeSpec::new(MATRIX_NAME)
        .instance_size(std::mem::size_of::<MatrixBody>())
        .attribute("determinant", get_determinant, None)
        .attribute("isMirrored", get_is_mirrored, None)
        .attribute("yaw", get_yaw, None)
        .attribute("pitch", get_pitch, None)
        .attribute("roll", get_roll, None)
        .attribute("translation", get_translation, Some(set_translation))
        .method("set", method_set)
        .method("setZero", method_set_zero)
        .method("setIdentity", method_set_identity)
        .method("setScale", method_set_scale)
        .method("setTranslate", method_set_translate)
        .method("setRotateX", method_set_rotate_x)
        .method("setRotateY", method_set_rotate_y)
        .method("setRotateZ", method_set_rotate_z)
        .method("setRotateYPR", method_set_rotate_ypr)
        .method("preMultiply", method_pre_multiply)
        .method("postMultiply", method_post_multiply)
        .method("invert", method_invert)
        .method("lookAt", method_look_at)
        .method("setElement", method_set_element)
        .method("get", method_get)
        .method("applyPoint", method_apply_point)
        .method("applyV4Point", method_apply_v4_point)
        .method("applyVector", method_apply_vector)
        .method("applyToAxis", method_apply_to_axis)
        .method("applyToOrigin", method_apply_to_origin)
        .method("orthogonalProjection", method_orthogonal_projection)
        .method("perspectiveProjection", method_perspective_projection)
        .method("__getstate__", method_get_state)
        .method("__setstate__", method_set_state)
        .slot(capi::Py_tp_new, matrix_new as *mut std::os::raw::c_void)
        .slot(capi::Py_tp_call, matrix_call as *mut std::os::raw::c_void)
}

// -----------------------------------------------------------------------------
// Section: Plane
// -----------------------------------------------------------------------------

/// The `Math.Plane` instance body: the plane equation. The reference
/// leaves both fields uninitialised until `init` runs
/// (`PyPlane`'s constructor is empty, `planeeq.ipp:14-16`); the port
/// zero-fills them instead, the deterministic stand-in for memory the
/// port cannot leave undefined.
struct PlaneBody {
    normal: [f32; 3],
    d: f32,
}

/// The `PyPlane` factory (`script_math.cpp:5279-5283`): the argument
/// tuple is never looked at, so any argument list builds a fresh plane.
///
/// # Safety
///
/// A C `tp_new` call: the interpreter holds the GIL.
unsafe extern "C" fn plane_new(
    type_object: *mut capi::PyTypeObject,
    _args: *mut capi::PyObject,
    _kwargs: *mut capi::PyObject,
) -> *mut capi::PyObject {
    let handle = PyRef::borrowed(type_object.cast());
    // SAFETY: the type is one of this module's and the interpreter is up.
    let Some(instance) = (unsafe { binding::instantiate(&handle) }) else {
        raise_type_error("Plane allocation failed");
        return std::ptr::null_mut();
    };
    own_raw(instance)
}

/// `Plane::init(p0, p1, p2)` with the default `SHOULD_NORMALISE`
/// (`planeeq.ipp:46-58`): the normal is the cross of the two edge
/// vectors, normalised, and `d` is its dot with the first point.
///
/// # Safety
///
/// A C method call; the interpreter holds the GIL.
unsafe extern "C" fn method_plane_init(
    self_: *mut capi::PyObject,
    args: *mut capi::PyObject,
) -> *mut capi::PyObject {
    if unsafe { capi::PyTuple_Size(args) } != 3 {
        arg_count_error("init", &["Vector3", "Vector3", "Vector3"]);
        return std::ptr::null_mut();
    }
    let mut parsed = [[0.0_f32; 3]; 3];
    for (index, point) in parsed.iter_mut().enumerate() {
        // SAFETY: the count check above bounds the index.
        let item = unsafe { capi::PyTuple_GetItem(args, index as isize) };
        let Some(value) = (unsafe { parse_vector_like(item, 3, &argument_name(index + 1)) }) else {
            return std::ptr::null_mut();
        };
        *point = [value[0], value[1], value[2]];
    }
    let edge1 = [
        parsed[1][0] - parsed[0][0],
        parsed[1][1] - parsed[0][1],
        parsed[1][2] - parsed[0][2],
    ];
    let edge2 = [
        parsed[2][0] - parsed[0][0],
        parsed[2][1] - parsed[0][1],
        parsed[2][2] - parsed[0][2],
    ];
    let mut normal = cross3(edge1, edge2);
    normalise3(&mut normal);
    // SAFETY: the caller guarantees the instance shape.
    let body = unsafe { &mut *body::<PlaneBody>(self_) };
    body.normal = normal;
    body.d = dot3(normal, parsed[0]);
    capi::py_none()
}

/// `PlaneEq::intersectRay` (`planeeq.ipp:248-253` with the half at
/// `:264-271`): the ray parameter is zero when the direction is exactly
/// parallel to the plane -- `isZero(float)` is the exact-zero test
/// (`stdmf_minimal.hpp:118-122`) -- otherwise the division; the answer is
/// `source + t * direction`.
///
/// # Safety
///
/// A C method call; the interpreter holds the GIL.
unsafe extern "C" fn method_plane_intersect_ray(
    self_: *mut capi::PyObject,
    args: *mut capi::PyObject,
) -> *mut capi::PyObject {
    if unsafe { capi::PyTuple_Size(args) } != 2 {
        arg_count_error("intersectRay", &["Vector3", "Vector3"]);
        return std::ptr::null_mut();
    }
    let mut parsed = [[0.0_f32; 3]; 2];
    for (index, point) in parsed.iter_mut().enumerate() {
        // SAFETY: the count check above bounds the index.
        let item = unsafe { capi::PyTuple_GetItem(args, index as isize) };
        let Some(value) = (unsafe { parse_vector_like(item, 3, &argument_name(index + 1)) }) else {
            return std::ptr::null_mut();
        };
        *point = [value[0], value[1], value[2]];
    }
    // SAFETY: the caller guarantees the instance shape.
    let body = unsafe { &*body::<PlaneBody>(self_) };
    let along = dot3(body.normal, parsed[1]);
    let t = if along == 0.0 {
        0.0_f32
    } else {
        (body.d - dot3(body.normal, parsed[0])) / along
    };
    let hit = [
        parsed[0][0] + t * parsed[1][0],
        parsed[0][1] + t * parsed[1][1],
        parsed[0][2] + t * parsed[1][2],
    ];
    own_raw(unsafe { vector_object([hit[0], hit[1], hit[2], 0.0], 3) })
}

/// The full `Math.Plane` type description: the constructor that ignores
/// its arguments and the two methods.
fn plane_spec() -> TypeSpec {
    TypeSpec::new(PLANE_NAME)
        .instance_size(std::mem::size_of::<PlaneBody>())
        .method("init", method_plane_init)
        .method("intersectRay", method_plane_intersect_ray)
        .slot(capi::Py_tp_new, plane_new as *mut std::os::raw::c_void)
}

/// The module-dictionary key of a dotted name.
fn bare_name(name: &'static str) -> std::ffi::CString {
    std::ffi::CString::new(name.strip_prefix("Math.").expect("the prefix"))
        .expect("no embedded NUL")
}

/// The `Math` module installation step for the matrix and plane classes:
/// the two `PyFactoryMethodLink` entries of the reference rung-0 jobs
/// (`py_factory_method_link.cpp:37-80`).
///
/// # Safety
///
/// The interpreter must be up on the calling thread.
pub(super) unsafe fn install(module: *mut capi::PyObject) {
    for (name, spec) in [(MATRIX_NAME, matrix_spec()), (PLANE_NAME, plane_spec())] {
        // SAFETY: the interpreter is up.
        let object = match binding::create_type(&spec) {
            Ok(object) => object,
            Err(_) => panic!("Math: the {name} type could not be created"),
        };
        remember_type(name, &object);
        // SAFETY: the module is live.
        unsafe { attach(module, bare_name(name), &object) };
        // The type object the registry of `binding` holds keeps everything
        // alive; this handle can go.
        drop(object);
    }
}

/// Attach one created type to the `Math` module dictionary under its bare
/// name -- the `PyFactoryMethodLink` step (`py_factory_method_link.cpp:37-80`).
///
/// # Safety
///
/// The interpreter must be up; `module` must be the `Math` module object.
unsafe fn attach(module: *mut capi::PyObject, name: std::ffi::CString, object: &PyRef) {
    // SAFETY: the module is a live module object and the name is a
    // NUL-terminated key.
    unsafe {
        capi::PyDict_SetItemString(
            capi::PyModule_GetDict(module),
            name.as_ptr(),
            object.as_ptr(),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A quarter turn around each axis, the reference's row layout: the
    /// axis vectors are rows, so a +90 degree X rotation sends the Y axis
    /// to the Z axis (`matrix.cpp:434-463`).
    #[test]
    fn a_quarter_turn_rows_the_axes() {
        let half_pi = std::f64::consts::FRAC_PI_2 as f32;
        let mut m = MatrixValue::identity();
        m.set_rotate_x(half_pi);
        let sent = m.apply_vector([0.0, 1.0, 0.0]);
        assert!((sent[2] - 1.0).abs() < 1e-6, "{sent:?}");

        let mut m = MatrixValue::identity();
        m.set_rotate_y(half_pi);
        let sent = m.apply_vector([0.0, 0.0, 1.0]);
        assert!((sent[0] - 1.0).abs() < 1e-6, "{sent:?}");

        let mut m = MatrixValue::identity();
        m.set_rotate_z(half_pi);
        let sent = m.apply_vector([1.0, 0.0, 0.0]);
        assert!((sent[1] - 1.0).abs() < 1e-6, "{sent:?}");
    }

    /// The composition order of `setRotate`: roll, then pitch, then yaw --
    /// equivalent to `setRotateY` followed by the two pre-rotations
    /// (`matrix.cpp:586-593`).
    #[test]
    fn set_rotate_composes_roll_pitch_yaw() {
        let yaw = 0.3_f32;
        let pitch = -0.7_f32;
        let roll = 1.1_f32;
        let mut composed = MatrixValue::identity();
        composed.set_rotate_ypr(yaw, pitch, roll);
        let mut manual = MatrixValue::identity();
        manual.set_rotate_y(yaw);
        manual.pre_rotate_x(pitch);
        manual.pre_rotate_z(roll);
        assert_eq!(composed.m, manual.m);
    }

    /// Pre-multiplication lands the argument on the left, post on the
    /// right; the translation pair shows it: `R * T` rotates the frame
    /// before the translation lands, so the origin rides the translation
    /// alone, while `T * R` translates first and the rotation swings the
    /// translated origin around the axis.
    #[test]
    fn pre_and_post_multiply_order_the_sides() {
        let mut rotate = MatrixValue::identity();
        rotate.set_rotate_y(std::f64::consts::FRAC_PI_2 as f32);
        let mut translate = MatrixValue::identity();
        translate.set_translate(1.0, 0.0, 0.0);

        let mut left = MatrixValue::identity();
        left.post_multiply(&translate);
        left.pre_multiply(&rotate);
        assert_eq!(left.apply_point([0.0, 0.0, 0.0]), [1.0, 0.0, 0.0]);

        let mut right = MatrixValue::identity();
        right.post_multiply(&translate);
        right.post_multiply(&rotate);
        let swung = right.apply_point([0.0, 0.0, 0.0]);
        // The f32 cosine of the quarter turn leaves an epsilon on x.
        assert!(swung[0].abs() < 1e-6, "{swung:?}");
        assert!((swung[1]).abs() < 1e-7);
        assert!((swung[2] + 1.0).abs() < 1e-6);
    }

    /// `invert` round-trips a rotate-and-translate; a singular matrix
    /// refuses and stays untouched (`matrix.cpp:614-628`).
    #[test]
    fn invert_round_trips_and_refuses_singular() {
        let mut m = MatrixValue::identity();
        m.set_rotate_y(0.9);
        m.set_translate(3.0, -2.0, 5.0);
        let original = m;
        assert!(m.invert());
        m.post_multiply(&original);
        for (cell, expect) in m.m.iter().flatten().zip([
            1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0,
        ]) {
            assert!((cell - expect).abs() < 1e-5, "{cell} vs {expect}");
        }

        let mut singular = MatrixValue::identity();
        singular.set_scale(0.0, 1.0, 1.0);
        let before = singular;
        assert!(!singular.invert());
        assert_eq!(singular.m, before.m);
    }

    /// The determinant is the 3x3 formula; a reflection is mirrored, a
    /// rotation is not (`matrix.cpp:130-136`).
    #[test]
    fn determinant_and_mirror_read_the_axes() {
        let mut m = MatrixValue::identity();
        m.set_scale(2.0, 3.0, 4.0);
        assert!((m.get_determinant() - 24.0).abs() < 1e-5);
        assert!(!m.is_mirrored());

        let mut mirror = MatrixValue::identity();
        mirror.m[0][0] = -1.0;
        assert!(mirror.is_mirrored());
    }

    /// `lookAt` from the origin towards +Z with world up: the direction
    /// lands in column two, the translation in the bottom row, and the
    /// untouched column-three cells keep whatever was there
    /// (`matrix.cpp:695-724`).
    #[test]
    fn look_at_points_the_columns_and_keeps_column_three() {
        let mut m = MatrixValue::zero();
        // The reference leaves these three cells alone: a garbage start
        // stays garbage. The zero start pins the fact.
        m.look_at([1.0, 2.0, 3.0], [0.0, 0.0, 1.0], [0.0, 1.0, 0.0]);
        assert!((m.m[0][2]).abs() < 1e-6);
        assert!((m.m[1][2]).abs() < 1e-6);
        assert!((m.m[2][2] - 1.0).abs() < 1e-6);
        assert!((m.m[3][0] + 1.0).abs() < 1e-6);
        assert!((m.m[3][1] + 2.0).abs() < 1e-6);
        assert!((m.m[3][2] + 3.0).abs() < 1e-6);
        assert!((m.m[3][3] - 1.0).abs() < 1e-6);
        assert_eq!(m.m[0][3], 0.0);
        assert_eq!(m.m[1][3], 0.0);
        assert_eq!(m.m[2][3], 0.0);
    }

    /// The yaw/pitch/roll of a composed rotation read back the inputs.
    #[test]
    fn yaw_pitch_roll_read_back() {
        let mut m = MatrixValue::identity();
        m.set_rotate_ypr(0.5, -0.25, 0.125);
        assert!((m.yaw() - 0.5).abs() < 1e-5, "{}", m.yaw());
        assert!((m.pitch() + 0.25).abs() < 1e-5, "{}", m.pitch());
        assert!((m.roll() - 0.125).abs() < 1e-5, "{}", m.roll());
    }

    /// The two projections set all sixteen cells; the perspective's
    /// cotangent goes through a double `tan`.
    #[test]
    fn projections_fill_their_cells() {
        let mut m = MatrixValue::identity();
        m.orthogonal_projection(4.0, 2.0, 1.0, 101.0);
        assert_eq!(m.m[0][0], 0.5);
        assert_eq!(m.m[1][1], 1.0);
        assert!((m.m[2][2] - 0.01).abs() < 1e-7);
        assert!((m.m[3][2] + 0.01).abs() < 1e-7);
        assert_eq!(m.m[3][3], 1.0);

        let mut p = MatrixValue::identity();
        p.perspective_projection(std::f64::consts::FRAC_PI_2 as f32, 1.0, 1.0, 101.0);
        assert!((p.m[0][0] - 1.0).abs() < 1e-6);
        assert!((p.m[1][1] - 1.0).abs() < 1e-6);
        assert!((p.m[2][2] - 1.01).abs() < 1e-5);
        assert_eq!(p.m[2][3], 1.0);
        assert!((p.m[3][2] + 1.01).abs() < 1e-5);
        assert_eq!(p.m[3][3], 0.0);
    }

    /// The `translation` setter writes the bottom row and its `w`.
    #[test]
    fn translation_writes_the_bottom_row() {
        let mut m = MatrixValue::zero();
        m.set_translation([7.0, 8.0, 9.0]);
        assert_eq!(m.m[3], [7.0, 8.0, 9.0, 1.0]);
    }

    /// The `__getstate__`/`__setstate__` pair round-trips the raw block.
    #[test]
    fn state_bytes_round_trip() {
        let mut m = MatrixValue::identity();
        m.set_translate(1.0, 2.0, 3.0);
        let bytes = m.state_bytes();
        assert_eq!(bytes.len(), 64);
        assert_eq!(MatrixValue::from_state_bytes(&bytes), m);
    }

    /// A plane through three points: the normal is the normalised cross of
    /// the edges and `d` its dot with the first point; the ray hit lands
    /// on the plane, and an exactly parallel ray answers the source.
    #[test]
    fn a_plane_answers_its_ray() {
        let mut body = PlaneBody {
            normal: [0.0; 3],
            d: 0.0,
        };
        let edge1 = [1.0_f32, 0.0, 0.0];
        let edge2 = [0.0_f32, 1.0, 0.0];
        body.normal = cross3(edge1, edge2);
        normalise3(&mut body.normal);
        body.d = dot3(body.normal, [0.0, 0.0, 5.0]);
        assert_eq!(body.normal, [0.0, 0.0, 1.0]);
        assert!((body.d - 5.0).abs() < 1e-6);

        // The ray straight down from ten units above lands on the plane.
        let along = dot3(body.normal, [0.0, 0.0, -1.0]);
        let t = if along == 0.0 {
            0.0
        } else {
            (body.d - dot3(body.normal, [0.0, 0.0, 10.0])) / along
        };
        let hit = [0.0, 0.0, 10.0 - t];
        assert!((hit[2] - 5.0).abs() < 1e-6, "{hit:?}");

        // A ray inside the plane: `isZero` is the exact test, the
        // parameter is zero, the answer is the source itself.
        let along = dot3(body.normal, [1.0, 0.0, 0.0]);
        let t = if along == 0.0 { 0.0 } else { 1.0 };
        assert_eq!(t, 0.0);
    }

    /// A colinear `init` leaves the normal at zero -- `normalise` leaves a
    /// near-zero vector untouched -- and `d` follows it.
    #[test]
    fn a_degenerate_plane_keeps_the_zero_normal() {
        let mut normal = cross3([1.0, 0.0, 0.0], [2.0, 0.0, 0.0]);
        normalise3(&mut normal);
        assert_eq!(normal, [0.0, 0.0, 0.0]);
        assert_eq!(dot3(normal, [3.0, 4.0, 5.0]), 0.0);
    }
}
