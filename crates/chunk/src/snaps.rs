//! BW-13-03-04 placement snapping: `tools/worldeditor/editor/snaps.cpp`.
//!
//! The snap grid/angle maths the WorldEditor applies to every placement and
//! transform. The float snapping keeps the reference's `fmodf` branches
//! (sign follows the dividend, like Rust's `%` on floats) so negative
//! coordinates snap onto the same grid the original editor produced.

use foundation::math::{MATH_PI, Matrix, Vector3, clamp};

/// `DEG_TO_RAD` (`mathdef.hpp`).
pub fn deg_to_rad(degrees: f32) -> f32 {
    degrees * MATH_PI / 180.0
}

/// `almostEqual(Vector3)` (`vector3.hpp:131`): every axis within 0.0004.
pub fn almost_equal(a: &Vector3, b: &Vector3) -> bool {
    const EPSILON: f32 = 0.0004;
    (a.x - b.x).abs() <= EPSILON && (a.y - b.y).abs() <= EPSILON && (a.z - b.z).abs() <= EPSILON
}

/// `Snap::value` (`snaps.cpp:26-47`): snap one float onto the grid. A grid
/// of zero or less leaves the value untouched; positive and negative values
/// walk opposite directions to land on the same lattice.
pub fn value(v: &mut f32, snap_size: f32) -> f32 {
    if snap_size > 0.0 {
        let half_snap = snap_size / 2.0;
        if *v > 0.0 {
            *v += half_snap;
            *v -= *v % snap_size;
        } else {
            *v -= half_snap;
            *v += (-*v) % snap_size;
        }
    }
    *v
}

/// `Snap::vector3` (`snaps.cpp:61-70`): snap each axis of `v` about `origin`.
pub fn vector3(v: &mut Vector3, snaps: &Vector3, origin: &Vector3) -> Vector3 {
    *v = *v + *origin;
    value(&mut v.x, snaps.x);
    value(&mut v.y, snaps.y);
    value(&mut v.z, snaps.z);
    *v = *v - *origin;
    *v
}

/// The const overload (`snaps.cpp:81-87`): snap a copy.
pub fn vector3_to(v: &Vector3, snaps: &Vector3, origin: &Vector3) -> Vector3 {
    let mut working = *v;
    vector3(&mut working, snaps, origin)
}

/// `Snap::angle` (`snaps.cpp:260-302`): snap a direction vector's yaw and
/// pitch onto multiples of `snap_angle` degrees, mirroring the yaw octants
/// for the -x hemisphere and rebuilding a unit vector.
pub fn angle(vector: &mut Vector3, snap_angle: f32) -> Vector3 {
    let mut yaw;
    let flip_yaw;
    if vector.x != 0.0 {
        yaw = (vector.z / vector.x).atan();
        flip_yaw = vector.x < 0.0;
    } else {
        yaw = MATH_PI / 2.0;
        flip_yaw = vector.z < 0.0;
    }
    let mut pitch = clamp(-1.0, vector.y, 1.0).acos();

    yaw = value(&mut yaw, deg_to_rad(snap_angle));
    pitch = value(&mut pitch, deg_to_rad(snap_angle));

    if flip_yaw {
        yaw -= MATH_PI;
    }

    let cos_yaw = yaw.cos();
    let sin_yaw = yaw.sin();
    let cos_pitch = pitch.cos();
    let sin_pitch = pitch.sin();

    *vector = Vector3::new(cos_yaw * sin_pitch, cos_pitch, sin_yaw * sin_pitch);
    vector.normalise();
    *vector
}

/// `Snap::angles` (`snaps.cpp:315-325`): snap each basis vector of a matrix.
pub fn angles(m: &mut Matrix, snap_size: f32) {
    if snap_size > 0.0 {
        for row in m.m.iter_mut().take(3) {
            let mut basis = Vector3::new(row[0], row[1], row[2]);
            angle(&mut basis, snap_size);
            row[0] = basis.x;
            row[1] = basis.y;
            row[2] = basis.z;
        }
    }
}

/// `Snap::satisfy` (`snaps.cpp:332-367`): an approximation of the least
/// common multiple of two snap sizes, working in hundredths so user snaps
/// such as 0.5 stay exact.
pub fn satisfy(a: f32, b: f32) -> f32 {
    if a > 0.0 {
        if b > 0.0 {
            let x = (a * 100.0) as i32;
            let y = (b * 100.0) as i32;
            let (x, y) = (x.max(y), x.min(y));
            if y == 0 || x % y == 0 {
                return x as f32 / 100.0;
            }
            // It'd be nice if we worked out the Least Common Multiple of x
            // and y here, rather than using this approximation.
            let mut t = x * y;
            while (t / 10) % x == 0 && (t / 10) % y == 0 {
                t /= 10;
            }
            return t as f32 / 100.0;
        }
        return a;
    }
    b
}

#[cfg(test)]
mod tests {
    use super::*;
    use foundation::math::Vector3;

    #[test]
    fn value_snaps_onto_the_positive_grid() {
        let mut v = 12.3;
        assert_eq!(value(&mut v, 10.0), 10.0);
        let mut v = 17.0;
        assert_eq!(value(&mut v, 10.0), 20.0);
        // snapSize of zero (or less) is a no-op
        let mut v = 12.3;
        assert_eq!(value(&mut v, 0.0), 12.3);
        let mut v = 12.3;
        assert_eq!(value(&mut v, -5.0), 12.3);
    }

    #[test]
    fn value_snaps_onto_the_negative_grid() {
        let mut v = -12.3;
        assert_eq!(value(&mut v, 10.0), -10.0);
        let mut v = -17.0;
        assert_eq!(value(&mut v, 10.0), -20.0);
    }

    #[test]
    fn vector3_snaps_each_axis_about_the_origin() {
        let snapped = vector3_to(
            &Vector3::new(12.3, -12.3, 4.9),
            &Vector3::new(10.0, 10.0, 5.0),
            &Vector3::ZERO,
        );
        assert_eq!(snapped, Vector3::new(10.0, -10.0, 5.0));
        // the origin offset lets chunk-local coordinates snap on the world grid
        let origin = Vector3::new(100.0, 0.0, 100.0);
        let snapped = vector3_to(
            &Vector3::new(37.3, 0.0, 21.4),
            &Vector3::new(10.0, 10.0, 10.0),
            &origin,
        );
        assert_eq!(snapped, Vector3::new(40.0, 0.0, 20.0));
    }

    #[test]
    fn angle_snaps_the_plus_x_hemisphere() {
        // +x rotated slightly (yaw about y: z/x ratio) snaps back onto +x
        let mut v = Vector3::new(1.0, 0.0, 0.1);
        v.normalise();
        angle(&mut v, 45.0);
        assert!((v.x - 1.0).abs() < 1e-5 && v.y.abs() < 1e-5 && v.z.abs() < 1e-5);
        // halfway between octants snaps onto the diagonal
        let mut v = Vector3::new(1.0, 0.0, 0.45);
        v.normalise();
        angle(&mut v, 45.0);
        assert!(v.x > 0.7 && v.z > 0.7);
        assert!((v.length() - 1.0).abs() < 1e-6);
    }

    #[test]
    fn angle_mirrors_the_minus_x_hemisphere() {
        let mut v = Vector3::new(-1.0, 0.0, -0.1);
        v.normalise();
        angle(&mut v, 45.0);
        assert!(v.x < -0.99 && v.z.abs() < 1e-4);
        // straight down snaps pitch to 180° and stays unit
        let mut v = Vector3::new(0.001, -1.0, 0.0);
        v.normalise();
        angle(&mut v, 45.0);
        assert!(v.y < -0.99);
    }

    #[test]
    fn angles_snaps_every_basis_vector() {
        let mut m = Matrix::identity();
        // rotate 10° about y: both bases sit 10° off a 45° grid axis
        m.pre_rotate_y(deg_to_rad(10.0));
        angles(&mut m, 45.0);
        let x_basis = Vector3::new(m.m[0][0], m.m[0][1], m.m[0][2]);
        let z_basis = Vector3::new(m.m[2][0], m.m[2][1], m.m[2][2]);
        assert!((x_basis - Vector3::new(1.0, 0.0, 0.0)).length() < 1e-5);
        assert!((z_basis - Vector3::new(0.0, 0.0, 1.0)).length() < 1e-5);
        // a zero snap size leaves the matrix alone
        let mut m = Matrix::identity();
        m.pre_rotate_y(deg_to_rad(10.0));
        let before = m;
        angles(&mut m, 0.0);
        assert_eq!(before.m[0][0], m.m[0][0]);
    }

    #[test]
    fn satisfy_approximates_the_least_common_multiple() {
        assert_eq!(satisfy(2.0, 4.0), 4.0);
        assert_eq!(satisfy(4.0, 2.0), 4.0);
        assert_eq!(satisfy(0.5, 1.5), 1.5);
        // one zero returns the other; both zero returns zero
        assert_eq!(satisfy(0.0, 3.0), 3.0);
        assert_eq!(satisfy(3.0, 0.0), 3.0);
        assert_eq!(satisfy(0.0, 0.0), 0.0);
        // the approximation strips common trailing zero pairs
        assert_eq!(satisfy(0.3, 0.7), 2.1);
    }

    #[test]
    fn deg_to_rad_turns_degrees_into_radians() {
        assert!((deg_to_rad(180.0) - MATH_PI).abs() < 1e-6);
        assert!((deg_to_rad(45.0) - MATH_PI / 4.0).abs() < 1e-6);
    }

    #[test]
    fn almost_equal_uses_the_reference_epsilon() {
        assert!(almost_equal(
            &Vector3::new(1.0, 2.0, 3.0),
            &Vector3::new(1.0003, 2.0, 3.0)
        ));
        assert!(!almost_equal(
            &Vector3::new(1.0, 2.0, 3.0),
            &Vector3::new(1.0005, 2.0, 3.0)
        ));
    }
}
