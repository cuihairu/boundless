//! BW-01-01-04/05 math primitives matching the reference scalar fallback.
//!
//! The reference builds its Linux server path without D3DX
//! (`lib/math/xp_math.hpp` non-`EXT_MATH` branch); every operation here
//! follows those formulas and float accumulation orders exactly.

pub mod angle;
pub mod boundbox;
pub mod convex_hull;
pub mod ema;
pub mod graham_scan;
pub mod lineeq;
pub mod lineeq3;
pub mod matrix;
pub mod plane;
pub mod portal2d;
pub mod quaternion;
pub mod sphere;
pub mod vector;

pub use angle::{MATH_PI, normalise as normalise_angle};
pub use boundbox::{BoundBox, RayIntersectionType};
pub use convex_hull::ConvexHull;
pub use ema::{AccumulatingEma, Accumulator, Ema};
pub use graham_scan::graham_scan;
pub use lineeq::LineEq;
pub use lineeq3::LineEq3;
pub use matrix::Matrix;
pub use plane::PlaneEq;
pub use portal2d::Portal2D;
pub use quaternion::Quaternion;
pub use sphere::Sphere;
pub use vector::{Vector2, Vector3, Vector4, almost_equal, almost_zero, clamp};
