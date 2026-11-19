//! BW-01-01-04/05 math primitives matching the reference scalar fallback.
//!
//! The reference builds its Linux server path without D3DX
//! (`lib/math/xp_math.hpp` non-`EXT_MATH` branch); every operation here
//! follows those formulas and float accumulation orders exactly.

pub mod angle;
pub mod boundbox;
pub mod matrix;
pub mod quaternion;
pub mod vector;

pub use angle::{normalise as normalise_angle, MATH_PI};
pub use boundbox::BoundBox;
pub use matrix::Matrix;
pub use quaternion::Quaternion;
pub use vector::{clamp, almost_zero, Vector2, Vector3, Vector4};
