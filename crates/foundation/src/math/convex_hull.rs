//! BW-10-02-05 the convex hull (`lib/math/convex_hull.{hpp,cpp}`): the
//! volume made up of the intersection of the negative half-space regions
//! of each plane.
//!
//! The reference borrows an external plane array (`ConvexHull(numPlanes,
//! PlaneEq* planes)`, `MF_ASSERT(numPlanes <= 32)`); the port owns the
//! planes in a `Vec` with the same cap asserted at construction.

use super::boundbox::BoundBox;
use super::plane::PlaneEq;
use super::vector::Vector3;

/// `ConvexHull` (`convex_hull.hpp:19-52`).
#[derive(Debug, Clone, Default)]
pub struct ConvexHull {
    planes: Vec<PlaneEq>,
}

impl ConvexHull {
    /// `ConvexHull(numPlanes, planes)` (`convex_hull.cpp:12-16`): the
    /// reference asserts `numPlanes <= 32` (the flag protocol carries one
    /// bit per plane in a `uint16`-backed mask walked as `uint32`).
    pub fn new(planes: Vec<PlaneEq>) -> Self {
        assert!(planes.len() <= 32, "reference asserts numPlanes <= 32");
        Self { planes }
    }

    /// `size` (`convex_hull.cpp:18-21`).
    pub fn size(&self) -> usize {
        self.planes.len()
    }

    /// `plane(index)` (`convex_hull.cpp:23-27`): the reference asserts the
    /// index is in range.
    pub fn plane(&self, index: usize) -> &PlaneEq {
        assert!(index < self.planes.len());
        &self.planes[index]
    }

    /// `intersects(center, radius)` (`convex_hull.cpp:29-34`): all planes
    /// enabled.
    pub fn intersects_sphere(&self, center: &Vector3, radius: f32) -> bool {
        let mut all_planes = !0u16;
        self.intersects_sphere_flags(center, radius, &mut all_planes)
    }

    /// `intersects(bb)` (`convex_hull.cpp:36-41`): all planes enabled.
    pub fn intersects_box(&self, bb: &BoundBox) -> bool {
        let mut all_planes = !0u16;
        self.intersects_box_flags(bb, &mut all_planes)
    }

    /// `intersects(center, radius, planeEqFlags)` (`convex_hull.cpp:43-68`):
    /// a plane whose distance puts the sphere fully inside clears its bit
    /// (subtree pruning); fully outside answers false with the flags
    /// untouched.
    pub fn intersects_sphere_flags(
        &self,
        center: &Vector3,
        radius: f32,
        plane_flags: &mut u16,
    ) -> bool {
        let mut cur_plane_bit: u32 = 1;
        for plane in &self.planes {
            if (*plane_flags as u32) & cur_plane_bit != 0 {
                let dist = plane.distance_to(center);
                if dist > radius {
                    // Then fully outside
                    return false;
                } else if dist < -radius {
                    // Then fully inside
                    *plane_flags &= !(cur_plane_bit as u16);
                }
            }
            cur_plane_bit <<= 1;
        }

        true
    }

    /// `intersects(bb, planeEqFlags)` (`convex_hull.cpp:82-139`): the
    /// negative-most corner per plane (`SelectFloatGE0` on the normal
    /// component) rejects first; the positive-most corner marks the plane
    /// as still interesting (`outFlags`). The reference only writes the
    /// flags back on the intersecting path — an early "fully outside"
    /// return leaves them untouched.
    pub fn intersects_box_flags(&self, bb: &BoundBox, plane_flags: &mut u16) -> bool {
        let min = &bb.min;
        let max = &bb.max;

        let mut cur_plane_bit: u32 = 1;
        let valid_planes_mask: u32 = (1u32 << self.planes.len()) - 1;
        let in_flags = (*plane_flags as u32) & valid_planes_mask;
        let mut out_flags: u32 = 0;

        while cur_plane_bit <= in_flags {
            if in_flags & cur_plane_bit != 0 {
                let plane = &self.planes[cur_plane_bit.trailing_zeros() as usize];

                let neg = Vector3::new(
                    if plane.normal().x >= 0.0 {
                        min.x
                    } else {
                        max.x
                    },
                    if plane.normal().y >= 0.0 {
                        min.y
                    } else {
                        max.y
                    },
                    if plane.normal().z >= 0.0 {
                        min.z
                    } else {
                        max.z
                    },
                );
                if plane.distance_to(&neg) >= 0.0 {
                    // Then fully outside
                    return false;
                }

                let pos = Vector3::new(
                    if plane.normal().x >= 0.0 {
                        max.x
                    } else {
                        min.x
                    },
                    if plane.normal().y >= 0.0 {
                        max.y
                    } else {
                        min.y
                    },
                    if plane.normal().z >= 0.0 {
                        max.z
                    } else {
                        min.z
                    },
                );
                if plane.distance_to(&pos) > 0.0 {
                    // Then we are intersecting
                    out_flags |= cur_plane_bit;
                }
            }
            cur_plane_bit <<= 1;
        }

        *plane_flags = out_flags as u16;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hull_construction_answers_size_and_planes() {
        let hull = ConvexHull::new(vec![
            PlaneEq::new(Vector3::new(0.0, 0.0, 1.0), 0.0),
            PlaneEq::new(Vector3::new(1.0, 0.0, 0.0), 3.0),
        ]);
        assert_eq!(hull.size(), 2);
        assert_eq!(hull.plane(1).d(), 3.0);
        assert!(std::panic::catch_unwind(|| hull.plane(2)).is_err());
        assert!(
            std::panic::catch_unwind(move || { ConvexHull::new(vec![PlaneEq::default(); 33]) })
                .is_err()
        );
    }

    #[test]
    fn sphere_intersections_follow_the_plane_distances() {
        // z >= 0 half space only.
        let hull = ConvexHull::new(vec![PlaneEq::new(Vector3::new(0.0, 0.0, 1.0), 0.0)]);

        // Fully outside: dist 5 > radius 1.
        assert!(!hull.intersects_sphere(&Vector3::new(0.0, 0.0, 5.0), 1.0));
        // Fully inside: dist -5 < -radius, the plane bit clears (the other
        // fifteen bits were never backed by planes and stay set).
        let mut flags = !0u16;
        assert!(hull.intersects_sphere_flags(&Vector3::new(0.0, 0.0, -5.0), 1.0, &mut flags));
        assert_eq!(flags, !1u16, "the fully-inside plane dropped out");
        // Straddling: dist 0.5 within the radius.
        assert!(hull.intersects_sphere(&Vector3::new(0.0, 0.0, 0.5), 1.0));
    }

    #[test]
    fn box_intersections_use_the_negative_corner_rejection() {
        // z >= 0 half space.
        let hull = ConvexHull::new(vec![PlaneEq::new(Vector3::new(0.0, 0.0, 1.0), 0.0)]);

        // Fully outside: the negative corner (min z) is above the plane.
        assert!(!hull.intersects_box(&BoundBox::new(
            Vector3::new(-1.0, -1.0, 2.0),
            Vector3::new(1.0, 1.0, 3.0)
        )));

        // Fully inside: the box sits below the plane, so no plane still
        // splits it and the bit clears.
        let mut flags = !0u16;
        assert!(hull.intersects_box_flags(
            &BoundBox::new(Vector3::new(-1.0, -1.0, -3.0), Vector3::new(1.0, 1.0, -1.0)),
            &mut flags
        ));
        assert_eq!(flags, 0);

        // Straddling: the positive corner pokes out, the bit stays.
        let mut flags = !0u16;
        assert!(hull.intersects_box_flags(
            &BoundBox::new(Vector3::new(-1.0, -1.0, -1.0), Vector3::new(1.0, 1.0, 2.0)),
            &mut flags
        ));
        assert_eq!(flags, 1);
    }

    #[test]
    fn flag_masks_limit_both_directions_of_the_walk() {
        let hull = ConvexHull::new(vec![
            PlaneEq::new(Vector3::new(0.0, 0.0, 1.0), 0.0),
            PlaneEq::new(Vector3::new(1.0, 0.0, 0.0), 0.0),
        ]);

        // A disabled plane is skipped entirely: the sphere is fully outside
        // the disabled x >= 0 plane yet the walk passes it through the
        // enabled z >= 0 plane (fully inside, bit clears).
        let mut flags = 1u16; // only plane 0 enabled
        assert!(hull.intersects_sphere_flags(&Vector3::new(10.0, 0.0, -5.0), 1.0, &mut flags));
        assert_eq!(flags, 0);

        // validPlanesMask clamps the incoming mask to the real plane count:
        // spurious high bits are dropped before the walk. The hull volume is
        // the negative half-spaces (x <= 0 and z <= 0); the slab sits fully
        // inside both.
        let mut flags = !0u16;
        assert!(hull.intersects_box_flags(
            &BoundBox::new(
                Vector3::new(-3.0, -1.0, -3.0),
                Vector3::new(-1.0, 1.0, -1.0)
            ),
            &mut flags
        ));
        assert_eq!(flags, 0, "both planes fully contain the slab");

        // A cleared low bit below a set high bit is skipped by the walk:
        // the octree hands children masks with fully-inside planes
        // cleared, so a box straddling only the x plane walks plane 1
        // with plane 0 disabled.
        let mut flags = 2u16; // plane 0 (z) disabled, plane 1 (x) enabled
        assert!(hull.intersects_box_flags(
            &BoundBox::new(Vector3::new(-3.0, -1.0, -3.0), Vector3::new(2.0, 1.0, -1.0)),
            &mut flags
        ));
        assert_eq!(flags, 2, "only the enabled plane wrote its bit back");

        // An early outside leaves the flags untouched (reference returns
        // before writing them back).
        let mut flags = !0u16;
        assert!(!hull.intersects_box_flags(
            &BoundBox::new(Vector3::new(5.0, 5.0, 5.0), Vector3::new(6.0, 6.0, 6.0)),
            &mut flags
        ));
        assert_eq!(flags, !0u16);
    }
}
