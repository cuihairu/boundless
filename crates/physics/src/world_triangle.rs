//! BW-10-03-04: the collision triangle (`lib/physics2/worldtri.hpp`).
//!
//! A triangle is three `Vector3` vertices plus a 16-bit flags word: the
//! low byte carries the collision flags, the high byte the material kind
//! (`worldtri.hpp:30-49`). On disk it is a raw 40-byte POD row (36 bytes
//! of vertices, flags, padding — `bsp.cpp:1188-1191`).

use foundation::math::{PlaneEq, Vector2, Vector3};

use crate::world_polygon::WorldPolygon;

/// `almostZero` (`stdmf.hpp:616-619`): the exclusive window
/// `-epsilon < f && f < epsilon`; the default epsilon is `0.0004`.
pub fn almost_zero(f: f32, epsilon: f32) -> bool {
    -epsilon < f && f < epsilon
}

/// `PlaneEq(v0, v1, v2, SHOULD_NOT_NORMALISE)` (`planeeq.ipp:117-128`):
/// the raw cross product is kept — the sweep's `ndt` fraction math relies
/// on the unnormalised length.
fn triangle_plane(triangle: &WorldTriangle) -> PlaneEq {
    let n = Vector3::ZERO.cross_product(
        &(triangle.v[1] - triangle.v[0]),
        &(triangle.v[2] - triangle.v[0]),
    );
    PlaneEq::new(n, n.dot_product(&triangle.v[0]))
}

/// `ISECT` (`worldtri.ipp:147-150`): the interval endpoints of one
/// triangle on the planes' shared line.
fn isect(vv0: f32, vv1: f32, vv2: f32, d0: f32, d1: f32, d2: f32) -> (f32, f32) {
    (
        vv0 + (vv1 - vv0) * d0 / (d0 - d1),
        vv0 + (vv2 - vv0) * d0 / (d0 - d2),
    )
}

/// `COMPUTE_INTERVALS` (`worldtri.ipp:157-186`): pick the edge crossing
/// the planes' shared line by the signs of the plane distances; the
/// coplanar tail (every distance almost zero) answers `None` — the
/// caller treats it as no intersection.
// The reference macro takes these eight scalars verbatim.
#[allow(clippy::too_many_arguments)]
fn compute_intervals(
    vv0: f32,
    vv1: f32,
    vv2: f32,
    d0: f32,
    d1: f32,
    d2: f32,
    d0d1: f32,
    d0d2: f32,
) -> Option<(f32, f32)> {
    if d0d1 > 0.0 {
        // Here we know that D0D2 <= 0.0: D0, D1 are on the same side, D2
        // on the other or on the plane.
        Some(isect(vv2, vv0, vv1, d2, d0, d1))
    } else if d0d2 > 0.0 {
        // Here we know that d0d1 <= 0.0.
        Some(isect(vv1, vv0, vv2, d1, d0, d2))
    } else if d1 * d2 > 0.0 || !almost_zero(d0, 0.0004) {
        // Here we know that d0d1 <= 0.0 or that D0 != 0.0.
        Some(isect(vv0, vv1, vv2, d0, d1, d2))
    } else if !almost_zero(d1, 0.0004) {
        Some(isect(vv1, vv0, vv2, d1, d0, d2))
    } else if !almost_zero(d2, 0.0004) {
        Some(isect(vv2, vv0, vv1, d2, d0, d1))
    } else {
        // Triangles are coplanar (treat as false).
        None
    }
}

/// `sort` (`worldtri.ipp:193-202`): swap when out of order.
fn sort_pair(a: f32, b: f32) -> (f32, f32) {
    if a > b { (b, a) } else { (a, b) }
}

/// `TRIANGLE_NOT_IN_BSP` — nothing ever collides with this triangle.
pub const TRIANGLE_NOT_IN_BSP: u16 = 0xff;
pub const TRIANGLE_CAMERANOCOLLIDE: u16 = 1 << 0;
pub const TRIANGLE_TRANSPARENT: u16 = 1 << 1;
pub const TRIANGLE_BLENDED: u16 = 1 << 2;
pub const TRIANGLE_TERRAIN: u16 = 1 << 3;
/// The player does not collide; the camera still does.
pub const TRIANGLE_NOCOLLIDE: u16 = 1 << 4;
pub const TRIANGLE_DOUBLESIDED: u16 = 1 << 5;
pub const TRIANGLE_WATER: u16 = 1 << 6;
pub const TRIANGLE_PROJECTILENOCOLLIDE: u16 = 1 << 7;

pub const TRIANGLE_COLLISIONFLAG_MASK: u16 = 0xff;
pub const TRIANGLE_MATERIALKIND_MASK: u16 = 0xff << 8;
/// How many bits to shift to get to the material-kind byte.
pub const TRIANGLE_MATERIALKIND_SHIFT: u32 = 8;

/// The convenience "do not collide against" flags
/// (`worldtri.hpp:139-140`).
pub const BW_TRIANGLE_NOCOLLIDE: u16 = TRIANGLE_NOCOLLIDE | TRIANGLE_WATER;

/// `sizeof(WorldTriangle)` — the on-disk row size the BSP format relies
/// on (`bsp.cpp:1188-1191`).
pub const WORLD_TRIANGLE_SIZE: usize = 40;

/// A triangle in the collision scene.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WorldTriangle {
    v: [Vector3; 3],
    flags: u16,
}

impl WorldTriangle {
    pub fn new(v0: Vector3, v1: Vector3, v2: Vector3, flags: u16) -> Self {
        Self {
            v: [v0, v1, v2],
            flags,
        }
    }

    pub fn v0(&self) -> Vector3 {
        self.v[0]
    }

    pub fn v1(&self) -> Vector3 {
        self.v[1]
    }

    pub fn v2(&self) -> Vector3 {
        self.v[2]
    }

    /// `v(index)` — `MF_ASSERT(index < 3)` (`worldtri.ipp:85-89`).
    pub fn v(&self, index: u32) -> Vector3 {
        assert!(index < 3, "reference MF_ASSERTs index < 3");
        self.v[index as usize]
    }

    /// `normal()` (`worldtri.ipp:95-98`): `(v2 - v0) x (v2 - v1)` — the
    /// reference vertex order, not the usual `(v1-v0) x (v2-v0)`.
    pub fn normal(&self) -> Vector3 {
        Vector3::ZERO.cross_product(&(self.v[2] - self.v[0]), &(self.v[2] - self.v[1]))
    }

    /// `WorldTriangle::intersects(start, dir, length)` — the
    /// Möller–Trumbore ray test (`worldtri.cpp:211-280`), double sided
    /// (no backface cull). `EPSILON = 0.000001` (`worldtri.cpp:208`);
    /// `almostZero(det, eps)` is the exclusive window `-eps < det < eps`
    /// (`stdmf.hpp:616-619`). On a hit `length` is narrowed to the hit
    /// fraction, so the caller's running value acts as both the search
    /// limit and the answer.
    pub fn intersects(&self, start: Vector3, dir: Vector3, length: &mut f32) -> bool {
        const EPSILON: f32 = 0.000_001;

        // Find the vectors for the edges sharing v0.
        let edge1 = self.v[1] - self.v[0];
        let edge2 = self.v[2] - self.v[0];

        // Begin calculating determinant — also used to calculate the U
        // parameter.
        let p = Vector3::ZERO.cross_product(&dir, &edge2);
        let det = edge1.dot_product(&p);

        // If the determinant is near zero, the ray lies in the plane of
        // the triangle (almostZero is the exclusive window).
        if -EPSILON < det && det < EPSILON {
            return false;
        }

        let inv_det = 1.0 / det;

        // Calculate the distance from vert0 to the ray origin.
        let t = start - self.v[0];

        // Calculate the U parameter and test bounds.
        let u = t.dot_product(&p) * inv_det;
        if u < 0.0 || 1.0 < u {
            return false;
        }

        // Prepare to test the V parameter.
        let q = Vector3::ZERO.cross_product(&t, &edge1);

        // Calculate the V parameter and test bounds.
        let v = dir.dot_product(&q) * inv_det;
        if v < 0.0 || 1.0 < u + v {
            return false;
        }

        // Calculate the intersection point = start + dist * dir.
        let distance_to_plane = edge2.dot_product(&q) * inv_det;

        if 0.0 < distance_to_plane && distance_to_plane < *length {
            *length = distance_to_plane;
            true
        } else {
            false
        }
    }

    /// `WorldTriangle::intersects(triangle)` (`worldtri.cpp:109-196`):
    /// static triangle-triangle overlap. Two non-coplanar triangles can
    /// only meet along the line their planes share; each triangle's
    /// interval on that line (projected onto the dominant axis of the
    /// plane normals' cross product) is computed and the intervals are
    /// compared. Coplanar pairs answer false
    /// (`COMPUTE_INTERVALS`' final branch, `worldtri.ipp:157-186`).
    pub fn intersects_triangle(&self, triangle: &WorldTriangle) -> bool {
        let tri_a = self;
        let tri_b = triangle;

        let plane_eq_a = triangle_plane(tri_a);
        let d_b0 = plane_eq_a.distance_to(&tri_b.v[0]);
        let d_b1 = plane_eq_a.distance_to(&tri_b.v[1]);
        let d_b2 = plane_eq_a.distance_to(&tri_b.v[2]);

        let d_b0d_b1 = d_b0 * d_b1;
        let d_b0d_b2 = d_b0 * d_b2;

        // Is triangle B on one side of the plane equation A.
        if d_b0d_b1 > 0.0 && d_b0d_b2 > 0.0 {
            return false;
        }

        let plane_eq_b = triangle_plane(tri_b);
        let d_a0 = plane_eq_b.distance_to(&tri_a.v[0]);
        let d_a1 = plane_eq_b.distance_to(&tri_a.v[1]);
        let d_a2 = plane_eq_b.distance_to(&tri_a.v[2]);

        let d_a0d_a1 = d_a0 * d_a1;
        let d_a0d_a2 = d_a0 * d_a2;

        // Is triangle A on one side of the plane equation B.
        if d_a0d_a1 > 0.0 && d_a0d_a2 > 0.0 {
            return false;
        }

        let d = Vector3::ZERO.cross_product(plane_eq_a.normal(), plane_eq_b.normal());

        let mut max = d.x.abs();
        let mut index = 0; // X_AXIS

        let temp = d.y.abs();
        if temp > max {
            max = temp;
            index = 1; // Y_AXIS
        }

        let temp = d.z.abs();
        if temp > max {
            index = 2; // Z_AXIS (the reference does not update max here)
        }

        let proj_a = Vector3::new(
            tri_a.v[0].get(index),
            tri_a.v[1].get(index),
            tri_a.v[2].get(index),
        );
        let proj_b = Vector3::new(
            tri_b.v[0].get(index),
            tri_b.v[1].get(index),
            tri_b.v[2].get(index),
        );

        // Compute interval for triangle A, then triangle B. A coplanar
        // pair (every distance almost zero) answers false.
        let (isect_a0, isect_a1) = match compute_intervals(
            proj_a.x, proj_a.y, proj_a.z, d_a0, d_a1, d_a2, d_a0d_a1, d_a0d_a2,
        ) {
            Some(v) => v,
            None => return false,
        };
        let (isect_b0, isect_b1) = match compute_intervals(
            proj_b.x, proj_b.y, proj_b.z, d_b0, d_b1, d_b2, d_b0d_b1, d_b0d_b2,
        ) {
            Some(v) => v,
            None => return false,
        };

        let (isect_a0, isect_a1) = sort_pair(isect_a0, isect_a1);
        let (isect_b0, isect_b1) = sort_pair(isect_b0, isect_b1);

        isect_a1 >= isect_b0 && isect_b1 >= isect_a0
    }

    /// `WorldTriangle::intersects(triangle, offset)` (`worldtri.cpp:295-486`):
    /// whether the prism swept by `triangle` along `offset` intersects this
    /// triangle. The fractions each vertex crosses this triangle's plane at
    /// select the efficient projected-2D path; anything degenerate
    /// (offset parallel to the plane, or the prism ends outside the plane)
    /// falls to the brute path that chops this triangle against the
    /// prism's five planes (`WorldPolygon::chop`).
    pub fn intersects_swept(&self, triangle: &WorldTriangle, offset: Vector3) -> bool {
        // first find the distance along 'translation' of the points of
        // 'triangle' from our plane eq.
        let plane_eq = triangle_plane(self);
        // `unitVector()` (`vector3.ipp:295-299`): a copy, normalised.
        let mut normal_unit = *plane_eq.normal();
        normal_unit.normalise();
        let mut offset_unit = offset;
        offset_unit.normalise();
        let unit_dot_product = normal_unit.dot_product(&offset_unit);
        let ndt = plane_eq.normal().length() * offset.length() * unit_dot_product;

        let mut vd0 = 0.0;
        let mut vd1 = 0.0;
        let mut vd2 = 0.0;

        let mut brute_check = false;
        // If the offset is parallel to the world triangles plane we do a
        // brute force check against the volume created by the triangle
        // prism, this is done because the efficient version of the
        // collision fails in this case.
        if almost_zero(unit_dot_product, 0.005) {
            brute_check = true;
        } else {
            let indt = 1.0 / ndt;
            // `intersectRayHalfNoCheck` (`planeeq.ipp:280-284`).
            vd0 = (plane_eq.d() - plane_eq.normal().dot_product(&triangle.v[0])) * indt;
            vd1 = (plane_eq.d() - plane_eq.normal().dot_product(&triangle.v[1])) * indt;
            vd2 = (plane_eq.d() - plane_eq.normal().dot_product(&triangle.v[2])) * indt;
            // If they are all too near or too far, then we consider the
            // volume to not cross the plane.
            if vd0 < 0.0 && vd1 < 0.0 && vd2 < 0.0 {
                return false;
            }
            if vd0 >= 1.0 && vd1 >= 1.0 && vd2 >= 1.0 {
                return false;
            }

            // If either end of the volume intersects planeEq we do a brute
            // force check against the volume, this is done because the
            // efficient version of the collision fails in this case.
            if vd0 < 0.0 || vd1 < 0.0 || vd2 < 0.0 || vd0 >= 1.0 || vd1 >= 1.0 || vd2 >= 1.0 {
                brute_check = true;
            }
        }

        if brute_check {
            // Get the start of the prism and make sure its plane is
            // pointing in the right direction.
            let mut pe1 = triangle_plane(triangle);
            let wt1 = if pe1.normal().dot_product(&offset) < 0.0 {
                let n = *pe1.normal();
                pe1 = PlaneEq::new(Vector3::new(-n.x, -n.y, -n.z), -pe1.d());
                WorldTriangle::new(triangle.v[2], triangle.v[1], triangle.v[0], triangle.flags)
            } else {
                *triangle
            };

            let mut wp = WorldPolygon::from_triangle(self.v[0], self.v[1], self.v[2]);

            // Check the first plane.
            if !wp.chop(&pe1) {
                return false;
            }

            // Create the second cap plane.
            let inv_normal = Vector3::new(-pe1.normal().x, -pe1.normal().y, -pe1.normal().z);
            let pe2 = PlaneEq::new(inv_normal, inv_normal.dot_product(&(wt1.v[0] + offset)));

            // Check against second cap plane.
            if !wp.chop(&pe2) {
                return false;
            }

            // Create the triangle at the other end of the prism.
            let wt2 =
                WorldTriangle::new(wt1.v[0] + offset, wt1.v[1] + offset, wt1.v[2] + offset, 0);

            // Check against the sides of the triangle prism
            // (`plane.init(p0, p1, p2, SHOULD_NOT_NORMALISE)`,
            // `planeeq.ipp:117-128`).
            for i in 0..3 {
                let p0 = wt1.v(i);
                let p1 = wt2.v(i);
                let p2 = wt1.v((i + 1) % 3);
                let n = Vector3::ZERO.cross_product(&(p1 - p0), &(p2 - p0));
                let plane = PlaneEq::new(n, n.dot_product(&p0));
                if !wp.chop(&plane) {
                    return false;
                }
            }

            // We intersect the triangle prism.
            true
        } else {
            // A more efficient version of checking: figure out a good
            // cartesian plane to project everything onto.
            let norm = *plane_eq.normal();
            let mut max = norm.x.abs();
            let (mut index_u, mut index_v) = (1usize, 2usize); // Y_AXIS, Z_AXIS

            let temp = norm.y.abs();
            if temp > max {
                max = temp;
                index_u = 0; // X_AXIS
                index_v = 2; // Z_AXIS
            }
            if norm.z.abs() > max {
                index_u = 0; // X_AXIS
                index_v = 1; // Y_AXIS
            }

            // Now project both triangles onto it. B's vertices ride the
            // prism to where each crosses this triangle's plane.
            let tri_b3d = [
                triangle.v[0] + offset * vd0,
                triangle.v[1] + offset * vd1,
                triangle.v[2] + offset * vd2,
            ];
            let tri_a = [
                Vector2::new(self.v[0].get(index_u), self.v[0].get(index_v)),
                Vector2::new(self.v[1].get(index_u), self.v[1].get(index_v)),
                Vector2::new(self.v[2].get(index_u), self.v[2].get(index_v)),
            ];
            let tri_b = [
                Vector2::new(tri_b3d[0].get(index_u), tri_b3d[0].get(index_v)),
                Vector2::new(tri_b3d[1].get(index_u), tri_b3d[1].get(index_v)),
                Vector2::new(tri_b3d[2].get(index_u), tri_b3d[2].get(index_v)),
            ];

            // `inside` (`worldtri.cpp:281-285`): the second vector is in
            // the half-plane on the anticlockwise side of the first
            // (colinear counts as inside).
            let inside = |va: Vector2, vb: Vector2| va.x * vb.y - va.y * vb.x >= 0.0;
            let sub = |a: Vector2, b: Vector2| Vector2::new(a.x - b.x, a.y - b.y);

            // We make sure at least one point of each triangle is on the
            // inside side of each segment of the other triangle.
            let sign_a = inside(sub(tri_a[1], tri_a[0]), sub(tri_a[2], tri_a[0]));
            let sign_b = inside(sub(tri_b[1], tri_b[0]), sub(tri_b[2], tri_b[0]));

            let mut i1 = 0usize;
            let mut i2 = 2usize;
            while i1 <= 2 {
                let segment_a = sub(tri_a[i1], tri_a[i2]);
                if inside(segment_a, sub(tri_b[0], tri_a[i2])) != sign_a
                    && inside(segment_a, sub(tri_b[1], tri_a[i2])) != sign_a
                    && inside(segment_a, sub(tri_b[2], tri_a[i2])) != sign_a
                {
                    return false;
                }

                let segment_b = sub(tri_b[i1], tri_b[i2]);
                if inside(segment_b, sub(tri_a[0], tri_b[i2])) != sign_b
                    && inside(segment_b, sub(tri_a[1], tri_b[i2])) != sign_b
                    && inside(segment_b, sub(tri_a[2], tri_b[i2])) != sign_b
                {
                    return false;
                }

                i2 = i1;
                i1 += 1;
            }

            // So they both have at least one point on the inside side of
            // every segment of the other triangle: they intersect.
            true
        }
    }

    pub fn flags(&self) -> u16 {
        self.flags
    }

    pub fn set_flags(&mut self, new_flags: u16) {
        self.flags = new_flags;
    }

    /// `WorldTriangle::bounce` (`worldtri.cpp:504-512`): reflect the
    /// velocity off the triangle's plane with the given restitution — the
    /// normal component is scaled by `-(1 + elasticity)`, the tangential
    /// component is kept.
    pub fn bounce(&self, v: &mut Vector3, elasticity: f32) {
        let mut normal = self.normal();
        normal.normalise();
        let proj = normal.dot_product(v);
        let scale = (1.0 + elasticity) * proj;
        *v = *v - normal * scale;
    }

    /// `collisionFlags(flags)` — the low byte (`worldtri.hpp:81-82`).
    pub fn collision_flags(&self) -> u16 {
        self.flags & TRIANGLE_COLLISIONFLAG_MASK
    }

    /// `materialKind(flags)` — the high byte (`worldtri.hpp:83-84`).
    pub fn material_kind(&self) -> u16 {
        (self.flags & TRIANGLE_MATERIALKIND_MASK) >> TRIANGLE_MATERIALKIND_SHIFT
    }

    /// `packFlags` — compose a flags word from the two bytes.
    pub fn pack_flags(collision_flags: i32, material_kind: i32) -> u16 {
        ((collision_flags & TRIANGLE_COLLISIONFLAG_MASK as i32)
            | (material_kind << TRIANGLE_MATERIALKIND_SHIFT)) as u16
    }

    /// Decode one 40-byte on-disk row: three vertices, flags, padding
    /// (padding is not kept — the reference `operator==` compares it, the
    /// port treats rows as equal when the visible fields are equal).
    pub fn from_row(row: &[u8]) -> Self {
        let vf = |i: usize| {
            Vector3::new(
                f32::from_le_bytes(row[i * 12..i * 12 + 4].try_into().unwrap()),
                f32::from_le_bytes(row[i * 12 + 4..i * 12 + 8].try_into().unwrap()),
                f32::from_le_bytes(row[i * 12 + 8..i * 12 + 12].try_into().unwrap()),
            )
        };
        Self {
            v: [vf(0), vf(1), vf(2)],
            flags: u16::from_le_bytes(row[36..38].try_into().unwrap()),
        }
    }

    /// Encode one 40-byte on-disk row (padding zeroed).
    pub fn to_row(&self) -> [u8; WORLD_TRIANGLE_SIZE] {
        let mut row = [0u8; WORLD_TRIANGLE_SIZE];
        for (i, v) in self.v.iter().enumerate() {
            row[i * 12..i * 12 + 4].copy_from_slice(&v.x.to_le_bytes());
            row[i * 12 + 4..i * 12 + 8].copy_from_slice(&v.y.to_le_bytes());
            row[i * 12 + 8..i * 12 + 12].copy_from_slice(&v.z.to_le_bytes());
        }
        row[36..38].copy_from_slice(&self.flags.to_le_bytes());
        row
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::panic::{AssertUnwindSafe, catch_unwind};

    fn v(x: f32, y: f32, z: f32) -> Vector3 {
        Vector3::new(x, y, z)
    }

    #[test]
    fn the_row_codec_round_trips_and_ignores_padding() {
        let tri = WorldTriangle::new(
            v(1.0, 2.0, 3.0),
            v(4.0, 5.0, 6.0),
            v(7.0, 8.0, 10.0),
            0x2A05,
        );
        let row = tri.to_row();
        assert_eq!(row.len(), WORLD_TRIANGLE_SIZE);
        assert_eq!(&row[38..40], &[0, 0], "padding is zeroed on encode");
        // Padding bytes on disk are not part of the port's equality.
        let mut padded = row;
        padded[38] = 0xab;
        padded[39] = 0xcd;
        assert_eq!(WorldTriangle::from_row(&padded), tri);
        assert_eq!(WorldTriangle::from_row(&row).to_row(), row);
    }

    #[test]
    fn normal_follows_the_reference_cross_order() {
        let tri = WorldTriangle::new(v(1.0, 2.0, 3.0), v(4.0, 5.0, 6.0), v(7.0, 8.0, 10.0), 0);
        // (v2 - v0) x (v2 - v1), the reference's vertex order.
        let a = v(6.0, 6.0, 7.0);
        let b = v(3.0, 3.0, 4.0);
        let expected = Vector3::ZERO.cross_product(&a, &b);
        assert_eq!(tri.normal(), expected);
        assert_eq!(expected.x, 3.0);
        assert_eq!(expected.y, -3.0);
        assert_eq!(expected.z, 0.0);
    }

    #[test]
    fn vertex_accessors_and_the_indexed_one_match_the_reference() {
        let tri = WorldTriangle::new(v(1.0, 1.0, 1.0), v(2.0, 2.0, 2.0), v(3.0, 3.0, 3.0), 0);
        assert_eq!(tri.v0(), v(1.0, 1.0, 1.0));
        assert_eq!(tri.v1(), v(2.0, 2.0, 2.0));
        assert_eq!(tri.v2(), v(3.0, 3.0, 3.0));
        assert_eq!(tri.v(0), tri.v0());
        assert_eq!(tri.v(1), tri.v1());
        assert_eq!(tri.v(2), tri.v2());
        assert!(catch_unwind(AssertUnwindSafe(|| tri.v(3))).is_err());
    }

    #[test]
    fn the_flags_word_splits_into_two_bytes() {
        let mut tri = WorldTriangle::new(v(0.0, 0.0, 0.0), v(1.0, 0.0, 0.0), v(0.0, 1.0, 0.0), 0);
        assert_eq!(tri.flags(), 0);
        assert_eq!(tri.collision_flags(), 0);
        assert_eq!(tri.material_kind(), 0);

        tri.set_flags(0x2A05);
        assert_eq!(tri.flags(), 0x2A05);
        assert_eq!(tri.collision_flags(), 0x05);
        assert_eq!(tri.material_kind(), 0x2A);
        assert_eq!(WorldTriangle::pack_flags(0x05, 0x2A), 0x2A05);
        // packFlags masks the collision byte like the reference.
        assert_eq!(WorldTriangle::pack_flags(0x105, 0x2A), 0x2A05);
    }

    #[test]
    fn the_ray_test_is_the_reference_moeller_trumbore() {
        // Vertical ray down through the triangle (z=1 plane); in-length 2,
        // hit narrows it to the plane distance 1 (worldtri.cpp:211-280).
        let tri = WorldTriangle::new(v(0.0, 0.0, 1.0), v(2.0, 0.0, 1.0), v(0.0, 2.0, 1.0), 0x0102);
        let mut length = 2.0;
        assert!(tri.intersects(v(0.5, 0.5, 3.0), v(0.0, 0.0, -4.0), &mut length));
        assert_eq!(length, 2.0 / 4.0);
        // Behind the origin: rejected (0 < distanceToPlane).
        let mut length = 2.0;
        assert!(!tri.intersects(v(0.5, 0.5, -3.0), v(0.0, 0.0, -4.0), &mut length));
        assert_eq!(length, 2.0);
        // Beyond the running length: rejected and the length is kept.
        let mut length = 0.1;
        assert!(!tri.intersects(v(0.5, 0.5, 3.0), v(0.0, 0.0, -4.0), &mut length));
        assert_eq!(length, 0.1);
        // Outside the u/v bounds.
        let mut length = 2.0;
        assert!(!tri.intersects(v(2.5, 2.5, 3.0), v(0.0, 0.0, -4.0), &mut length));
        // Parallel ray (det in the epsilon window): rejected.
        let mut length = 2.0;
        assert!(!tri.intersects(v(0.5, 0.5, 1.0), v(1.0, 0.0, 0.0), &mut length));
        // Double sided: the same geometry from below hits too.
        let mut length = 2.0;
        assert!(tri.intersects(v(0.5, 0.5, -3.0), v(0.0, 0.0, 4.0), &mut length));
        assert_eq!(length, 1.0);
        // Edge-inclusive bounds: u == 1 exactly is inside.
        let mut length = 5.0;
        assert!(tri.intersects(v(2.0, 0.0, 3.0), v(0.0, 0.0, -4.0), &mut length));
        assert_eq!(length, 0.5);
    }

    #[test]
    fn the_nocollide_pair_combines_player_and_water_bits() {
        assert_eq!(BW_TRIANGLE_NOCOLLIDE, TRIANGLE_NOCOLLIDE | TRIANGLE_WATER);
        assert_eq!(BW_TRIANGLE_NOCOLLIDE, 0x50);
        assert_eq!(TRIANGLE_NOT_IN_BSP, 0xff);
    }

    fn face() -> WorldTriangle {
        WorldTriangle::new(v(0.0, 0.0, 0.0), v(2.0, 0.0, 0.0), v(0.0, 2.0, 0.0), 0)
    }

    #[test]
    fn the_static_overlap_test_answers_crossings_and_separations() {
        let a = face();
        // A triangle crossing a's z=0 plane inside its extent.
        let crossing = WorldTriangle::new(v(0.5, 0.5, -1.0), v(0.5, 0.5, 1.0), v(2.0, 2.0, 1.0), 0);
        assert!(a.intersects_triangle(&crossing));
        // The same answer from the other side (the reference construction
        // is symmetric in its two operands).
        assert!(crossing.intersects_triangle(&a));
        // Entirely beyond the other's plane.
        let above = WorldTriangle::new(v(0.0, 0.0, 1.0), v(2.0, 0.0, 1.0), v(0.0, 2.0, 1.0), 0);
        assert!(!a.intersects_triangle(&above));
        // Separated in space.
        let far = WorldTriangle::new(
            v(10.0, 10.0, -1.0),
            v(10.0, 10.0, 1.0),
            v(12.0, 12.0, 1.0),
            0,
        );
        assert!(!a.intersects_triangle(&far));
    }

    #[test]
    fn the_static_overlap_test_rejects_coplanar_triangles() {
        // A coplanar triangle strictly inside `face` — the reference's
        // COMPUTE_INTERVALS coplanar tail answers false even here.
        let a = face();
        let inside = WorldTriangle::new(v(0.5, 0.0, 0.0), v(1.5, 0.0, 0.0), v(0.5, 1.0, 0.0), 0);
        assert!(!a.intersects_triangle(&inside));
    }

    #[test]
    fn the_swept_test_crosses_with_the_efficient_path() {
        let a = face();
        // A parallel triangle swept down through a's plane: every vertex
        // crosses at fraction 2/3, the projected slices overlap.
        let high = WorldTriangle::new(v(-0.5, -0.5, 3.0), v(2.5, -0.5, 3.0), v(-0.5, 2.5, 3.0), 0);
        assert!(a.intersects_swept(&high, v(0.0, 0.0, -4.5)));
        // Swept away above the plane: every vertex crosses beyond the
        // prism (all fractions >= 1).
        assert!(!a.intersects_swept(&high, v(0.0, 0.0, 10.0)));
        // Below the plane, swept further down (all fractions < 0).
        let low = WorldTriangle::new(
            v(-0.5, -0.5, -1.0),
            v(2.5, -0.5, -1.0),
            v(-0.5, 2.5, -1.0),
            0,
        );
        assert!(!a.intersects_swept(&low, v(0.0, 0.0, -1.0)));
    }

    #[test]
    fn the_swept_end_cap_on_the_plane_answers_miss() {
        let a = face();
        // The prism's end cap lands exactly on a's plane: every crossing
        // fraction is exactly 1 and the volume is treated as not crossing.
        let above = WorldTriangle::new(v(-0.5, -0.5, 1.0), v(2.5, -0.5, 1.0), v(-0.5, 2.5, 1.0), 0);
        assert!(!a.intersects_swept(&above, v(0.0, 0.0, -1.0)));
        // The start cap exactly ON the plane (coplanar) still counts as
        // crossing: the 2D projections overlap, so this is a hit — unlike
        // the static coplanar test.
        let coplanar = WorldTriangle::new(v(0.5, 0.0, 0.0), v(1.5, 0.0, 0.0), v(0.5, 1.0, 0.0), 0);
        assert!(a.intersects_swept(&coplanar, v(0.0, 0.0, -1.0)));
    }

    #[test]
    fn the_swept_brute_path_chops_through_mixed_fractions() {
        let a = face();
        // Vertices on both sides of a's plane (fractions 0.125 and
        // negative) force the brute chop path; the prism genuinely
        // contains part of `a` (e.g. (0.5, 0.5, 0)).
        let straddling =
            WorldTriangle::new(v(0.3, 0.3, -0.5), v(1.5, 0.2, 0.4), v(0.2, 1.5, 0.2), 0);
        assert!(a.intersects_swept(&straddling, v(0.0, 0.0, 1.0)));
        // Offset parallel to a's plane: the brute path slices `a` against
        // the offset-parallel caps and finds nothing three-dimensional.
        let sliding = WorldTriangle::new(v(0.5, 0.5, 0.3), v(2.0, 0.5, 0.3), v(0.5, 2.0, 0.3), 0);
        assert!(!a.intersects_swept(&sliding, v(1.0, 0.0, 0.0)));
        // The degenerate zero offset against the identical triangle: every
        // chop keeps everything (all distances exactly 0), so the answer
        // is a hit.
        assert!(a.intersects_swept(&a, v(0.0, 0.0, 0.0)));
    }

    #[test]
    fn the_interval_helpers_follow_the_reference_macro() {
        // ISECT (worldtri.ipp:147-150): both crossings of the shared line.
        let (i0, i1) = isect(0.0, 1.0, 2.0, -0.39, 0.21, 0.21);
        assert_eq!(i0, 0.0 + (1.0 - 0.0) * -0.39 / (-0.39 - 0.21));
        assert_eq!(i1, 0.0 + (2.0 - 0.0) * -0.39 / (-0.39 - 0.21));
        // sort (worldtri.ipp:193-202): swap only when out of order.
        assert_eq!(sort_pair(2.0, 1.0), (1.0, 2.0));
        assert_eq!(sort_pair(1.0, 2.0), (1.0, 2.0));
        // almostZero (stdmf.hpp:616-619): the exclusive window.
        assert!(almost_zero(0.0, 0.0004));
        assert!(!almost_zero(0.0004, 0.0004));
        assert!(!almost_zero(-0.0004, 0.0004));
        assert!(almost_zero(0.00039, 0.0004));
        // COMPUTE_INTERVALS' coplanar tail: all distances almost zero.
        assert_eq!(
            compute_intervals(0.0, 1.0, 2.0, 0.0, 0.0, 0.0, 0.0, 0.0),
            None
        );
    }

    #[test]
    fn triangle_plane_keeps_the_raw_cross_product() {
        // SHOULD_NOT_NORMALISE: the normal is the raw (0,0,4) cross and d
        // is its dot with v0 (planeeq.ipp:117-128).
        let plane = triangle_plane(&face());
        assert_eq!(*plane.normal(), v(0.0, 0.0, 4.0));
        assert_eq!(plane.d(), 0.0);
        let offset = WorldTriangle::new(v(0.0, 0.0, 1.0), v(2.0, 0.0, 1.0), v(0.0, 2.0, 1.0), 0);
        assert_eq!(triangle_plane(&offset).d(), 4.0);
    }

    #[test]
    fn the_static_overlap_walk_picks_the_y_axis_when_the_cross_says_so() {
        // cross(nA, nB) = (0, -8, 0): the Y branch wins (the reference
        // updates max here), and a's distances to b's plane (2, -2, 2)
        // take the d0d2 > 0 COMPUTE_INTERVALS branch.
        let a = WorldTriangle::new(v(0.0, 0.0, 0.0), v(2.0, 0.0, 0.0), v(0.0, 2.0, 0.0), 0);
        let b = WorldTriangle::new(v(0.5, 0.5, -1.0), v(1.5, 0.5, 1.0), v(1.5, 1.5, 1.0), 0);
        assert!(a.intersects_triangle(&b));
    }

    #[test]
    fn the_static_overlap_walk_picks_the_z_axis_without_updating_max() {
        // cross(nA, nB) = (0, -8, 24): |z| > max so the Z branch assigns
        // the axes without touching max (worldtri.cpp:169-170).
        let a = WorldTriangle::new(v(0.0, -1.0, 0.0), v(0.0, 1.0, 0.0), v(0.0, 0.0, -2.0), 0);
        let b = WorldTriangle::new(v(-1.0, 0.2, 0.0), v(1.0, 0.2, -2.0), v(-1.0, 1.2, -3.0), 0);
        assert!(a.intersects_triangle(&b));
    }

    #[test]
    fn the_static_overlap_test_rejects_when_only_b_goes_coplanar() {
        // b's vertices all sit within the 0.0004 window of a's plane (and
        // none of the d0d1/d0d2 products go positive), so b's interval is
        // the coplanar None while a's is a real interval.
        let a = WorldTriangle::new(v(0.0, 0.0, 0.0), v(1.0, 0.0, 0.0), v(0.0, 1.0, 0.0), 0);
        let b = WorldTriangle::new(
            v(-10.0, -10.0, 0.0003),
            v(10.0, 0.0, -0.0003),
            v(0.0, 10.0, 0.0),
            0,
        );
        assert!(!a.intersects_triangle(&b));
    }

    #[test]
    fn the_static_overlap_walk_crosses_through_a_degenerate_third_distance() {
        // b has two vertices exactly on a's plane and one below it: its
        // interval falls through d1*d2 <= 0 and the almost-zero d0/d1 to
        // the !almostZero(d2) COMPUTE_INTERVALS branch.
        let a = WorldTriangle::new(v(0.0, 0.0, 0.0), v(1.0, 0.0, 0.0), v(0.0, 1.0, 0.0), 0);
        let b = WorldTriangle::new(v(0.5, 0.5, 0.0), v(1.5, 0.5, 0.0), v(0.5, 1.5, -1.0), 0);
        assert!(a.intersects_triangle(&b));
    }

    #[test]
    fn the_swept_projection_picks_the_y_axis_when_the_normal_says_so() {
        // a's plane normal is (0,1,0): |ny| > |nx| selects the (X,Z)
        // projection; b's slice (fraction 0.8 of the way down) lands
        // inside a's (x,z) footprint.
        let a = WorldTriangle::new(v(0.0, 0.0, 0.0), v(1.0, 0.0, 1.0), v(0.0, 0.0, -1.0), 0);
        let b = WorldTriangle::new(v(0.2, 2.0, 0.2), v(0.7, 2.0, 0.2), v(0.2, 2.0, 0.7), 0);
        assert!(a.intersects_swept(&b, v(0.0, -2.5, 0.0)));
    }

    #[test]
    fn the_swept_brute_path_flips_a_back_facing_prism_cap() {
        // b's cap plane opposes the offset (normal . offset < 0): the
        // brute path negates the plane and walks b's vertices in reverse.
        // The prism's side planes then collapse a's polygon to the
        // y = 0.1 line and the chop answers no.
        let a = WorldTriangle::new(v(0.0, 0.0, 0.0), v(1.0, 0.0, 0.0), v(0.0, 1.0, 0.0), 0);
        let b = WorldTriangle::new(v(0.5, 0.1, 0.0), v(1.5, 0.1, 1.0), v(0.5, 0.9, 1.0), 0);
        assert!(!a.intersects_swept(&b, v(1.0, 0.0, 0.0)));
    }

    #[test]
    fn the_swept_brute_path_stops_at_the_far_cap_plane() {
        // b's prism sits at x in [-6, -4]: the flipped near cap keeps a's
        // polygon alive but the far cap plane clears it entirely.
        let a = WorldTriangle::new(v(0.0, 0.0, 0.0), v(1.0, 0.0, 0.0), v(0.0, 1.0, 0.0), 0);
        let b = WorldTriangle::new(v(-5.0, 0.2, 0.0), v(-6.0, 0.2, 1.0), v(-5.0, 0.8, 1.0), 0);
        assert!(!a.intersects_swept(&b, v(1.0, 0.0, 0.0)));
    }

    #[test]
    fn the_swept_slice_far_from_the_triangle_rejects_on_a_segment() {
        // b's slice lands at x,y in [5,6], far outside a: some segment of
        // a sees all three slice vertices on its outside.
        let a = WorldTriangle::new(v(0.0, 0.0, 0.0), v(1.0, 0.0, 0.0), v(0.0, 1.0, 0.0), 0);
        let b = WorldTriangle::new(v(5.0, 5.0, 1.0), v(6.0, 5.0, 1.0), v(5.0, 6.0, 1.0), 0);
        assert!(!a.intersects_swept(&b, v(0.0, 0.0, -1.5)));
    }

    #[test]
    fn the_swept_slice_rejects_on_a_segment_after_b_segments_pass() {
        // The slice sits at x >= 4, beyond a. B's own b0-b2 edge (x = 5)
        // has a1 on its interior side, and b's y = 0.4 edge sees a2
        // across it, so no segment of b rejects; the x+y = 1 line of a's
        // a2-a1 edge has all three slice vertices on its outside, and
        // that segment_a test fires at the last iteration.
        let a = WorldTriangle::new(v(0.0, 0.0, 0.0), v(1.0, 0.0, 0.0), v(0.0, 1.0, 0.0), 0);
        let b = WorldTriangle::new(v(5.0, 0.4, 1.0), v(4.0, 0.4, 1.0), v(5.0, 1.4, 1.0), 0);
        assert!(!a.intersects_swept(&b, v(0.0, 0.0, -1.5)));
    }

    #[test]
    fn the_swept_slice_rejects_on_b_segment_after_a_segments_pass() {
        // The slice sits at x+y <= -3, below a. A's x >= 0 edge keeps a
        // slice vertex inside (b2 at x = 1), so no segment of a rejects;
        // the line of b's b0-b2 edge (x+y = -3) has all three vertices
        // of a on its outside, and that segment_b test fires at the
        // first iteration.
        let a = WorldTriangle::new(v(0.0, 0.0, 0.0), v(10.0, 0.0, 0.0), v(0.0, 10.0, 0.0), 0);
        let b = WorldTriangle::new(v(-2.0, -1.0, 1.0), v(-4.0, -3.0, 1.0), v(1.0, -4.0, 1.0), 0);
        assert!(!a.intersects_swept(&b, v(0.0, 0.0, -1.5)));
    }

    /// `bounce` (`worldtri.cpp:504-512`): restitution 1 reflects the
    /// normal component exactly; restitution 0.3 keeps 30% of the
    /// reflected normal component; the tangential component is untouched.
    #[test]
    fn bounce_reflects_the_velocity_about_the_plane() {
        // An upward-facing floor: normal() = (v2-v0) x (v2-v1) = +y.
        let floor = WorldTriangle::new(v(0.0, 0.0, 0.0), v(1.0, 0.0, 0.0), v(0.0, 0.0, -1.0), 0);
        assert!((floor.normal().y - 1.0).abs() < 1e-5, "upward winding");

        // Falling straight down, perfectly elastic: straight back up.
        let mut velocity = v(0.0, -2.0, 0.0);
        floor.bounce(&mut velocity, 1.0);
        assert!(velocity.x.abs() < 1e-6 && velocity.z.abs() < 1e-6);
        assert!((velocity.y - 2.0).abs() < 1e-5, "y = {}", velocity.y);

        // A skew approach: the tangential part survives, the normal part
        // is reflected and damped: (1, -2, 0) -> (1, 2*0.3, 0).
        let mut velocity = v(1.0, -2.0, 0.0);
        floor.bounce(&mut velocity, 0.3);
        assert!((velocity.x - 1.0).abs() < 1e-6);
        assert!((velocity.y - 0.6).abs() < 1e-5, "y = {}", velocity.y);
    }
}
