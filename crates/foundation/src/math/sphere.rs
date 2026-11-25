//! BW-10-02-02 spheres (`lib/math/sphere.hpp/.cpp`).
//!
//! Only the data face needed by compiled-space scene queries is carried:
//! center + radius and the AABB construction the static scene writer uses
//! when it records per-object bound spheres.

use super::boundbox::BoundBox;
use super::vector::Vector3;

/// `center_` + `radius_`, streamed as three little-endian f32s plus one
/// (`sphere.hpp` member layout).
pub const SPHERE_STREAM_SIZE: usize = 16;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Sphere {
    pub center: Vector3,
    pub radius: f32,
}

impl Sphere {
    pub const fn new(center: Vector3, radius: f32) -> Self {
        Self { center, radius }
    }

    /// `sphere.cpp:26-31`: the center is the box centre and the radius is
    /// half the diagonal length.
    pub fn from_bound_box(bb: &BoundBox) -> Self {
        Self {
            center: bb.centre(),
            radius: (bb.max - bb.min).length() / 2.0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sphere_stream_size_is_center_plus_radius() {
        assert_eq!(SPHERE_STREAM_SIZE, 16);
    }

    #[test]
    fn from_bound_box_takes_centre_and_half_diagonal() {
        let bb = BoundBox::new(Vector3::new(-1.0, -1.0, -1.0), Vector3::new(1.0, 1.0, 1.0));
        let sphere = Sphere::from_bound_box(&bb);
        assert_eq!(sphere.center, Vector3::ZERO);
        assert!((sphere.radius - (12.0f32.sqrt() / 2.0)).abs() < 1e-6);
    }

    #[test]
    fn from_bound_box_on_flat_box_gives_half_extent() {
        let bb = BoundBox::new(Vector3::new(0.0, 4.0, 0.0), Vector3::new(20.0, 6.0, 20.0));
        let sphere = Sphere::from_bound_box(&bb);
        assert_eq!(sphere.center, Vector3::new(10.0, 5.0, 10.0));
        // half diagonal of (20, 2, 20) = sqrt(804) / 2 (400+4+400)
        assert!((sphere.radius - (804.0f32.sqrt() / 2.0)).abs() < 1e-5);
    }
}
