//! Provider layer for the `Math` module (BW-03-03-02): `MatrixProduct`,
//! `MatrixInverse`, the `Vector4Provider` family and `Math.getRegister`.
//!
//! This file is the landing spot for the port of the reference's provider
//! classes (`lib/pyscript/script_math.cpp`). It is intentionally still a
//! stub: the module hook and the matrix-side dispatch are wired up so the
//! crate builds, and the concrete provider types are filled in as the port
//! proceeds. The dispatch answers `None`, so `matrix::provider_matrix`
//! reduces to the value read -- the reference's behaviour before a provider
//! object is actually attached.

use super::PyRef;
use super::binding;
use super::capi;
use foundation::math::Matrix;

/// Register the provider types and the `Math` factories. Sibling of
/// `matrix::register`; the result is discarded by `math::register`, so the
/// stub's empty success is indistinguishable from the reference's
/// fire-and-forget init-time job.
pub(super) fn register() -> Result<PyRef, binding::BindError> {
    Ok(PyRef::empty())
}

/// The matrix-side dispatch `matrix::provider_matrix` falls through to for a
/// provider object that is not a `Matrix` value (`script_math.cpp`'s
/// `MatrixProvider::matrix` vtable entry). The stub answers `None`, so
/// `provider_matrix` reduces to the value read until the providers land.
#[expect(dead_code, reason = "placeholder until the provider vtable lands")]
pub(super) fn provider_matrix_dispatch(_object: *mut capi::PyObject) -> Option<Matrix> {
    None
}
