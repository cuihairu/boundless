//! BW-01-01-05 axis-aligned bounding boxes (`lib/math/boundbox.hpp/.cpp`).

use super::matrix::Matrix;
use super::vector::Vector3;

/// `min` starts at `FLT_MAX`, `max` at `-FLT_MAX`: the default box is
/// inside-out (`boundbox.ipp:10-14`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BoundBox {
    pub min: Vector3,
    pub max: Vector3,
}

impl Default for BoundBox {
    fn default() -> Self {
        Self {
            min: Vector3::new(f32::MAX, f32::MAX, f32::MAX),
            max: Vector3::new(f32::MIN, f32::MIN, f32::MIN),
        }
    }
}

impl BoundBox {
    pub fn new(min: Vector3, max: Vector3) -> Self {
        Self { min, max }
    }

    /// Any component with `min > max` (`boundbox.ipp:331-334`).
    pub fn inside_out(&self) -> bool {
        self.min.x > self.max.x || self.min.y > self.max.y || self.min.z > self.max.z
    }

    /// Grow to include a point; an inside-out box snaps to the point
    /// (`boundbox.ipp:154-180`).
    pub fn add_bounds(&mut self, v: &Vector3) {
        if self.inside_out() {
            self.min = *v;
            self.max = *v;
        } else {
            self.min.x = self.min.x.min(v.x);
            self.min.y = self.min.y.min(v.y);
            self.min.z = self.min.z.min(v.z);
            self.max.x = self.max.x.max(v.x);
            self.max.y = self.max.y.max(v.y);
            self.max.z = self.max.z.max(v.z);
        }
    }

    /// Point containment is **half-open**: `>= min && < max` on every axis
    /// (`boundbox.ipp:303-307`).
    pub fn contains(&self, v: &Vector3) -> bool {
        v.x >= self.min.x
            && v.x < self.max.x
            && v.y >= self.min.y
            && v.y < self.max.y
            && v.z >= self.min.z
            && v.z < self.max.z
    }

    /// Box overlap uses the closed interval `min2 <= max1 && min1 <= max2`
    /// (`boundbox.ipp:285-297`).
    pub fn intersects(&self, other: &Self) -> bool {
        other.min.x <= self.max.x
            && self.min.x <= other.max.x
            && other.min.y <= self.max.y
            && self.min.y <= other.max.y
            && other.min.z <= self.max.z
            && self.min.z <= other.max.z
    }

    /// Grow to include another box: an inside-out source is skipped, an
    /// inside-out `self` snaps to it (`boundbox.ipp:211-231`).
    pub fn add_bound_box(&mut self, bb: &Self) {
        if bb.inside_out() {
            return;
        }
        if self.inside_out() {
            self.min = bb.min;
            self.max = bb.max;
        } else {
            self.min.x = self.min.x.min(bb.min.x);
            self.min.y = self.min.y.min(bb.min.y);
            self.min.z = self.min.z.min(bb.min.z);
            self.max.x = self.max.x.max(bb.max.x);
            self.max.y = self.max.y.max(bb.max.y);
            self.max.z = self.max.z.max(bb.max.z);
        }
    }

    /// `(min + max) / 2.f` (`boundbox.ipp:323-328`).
    pub fn centre(&self) -> Vector3 {
        (self.min + self.max) / 2.0
    }

    pub fn width(&self) -> Vector3 {
        self.max - self.min
    }

    /// `AABB::clip` (`boundbox.cpp:310-388`): clip the segment `source` →
    /// `extent` against this box bloated by `bloat` in all directions.
    /// `isZero(delta)` is the reference's *exact* zero test
    /// (`stdmf_minimal.hpp:110-139`, `isEqual(f, 0)` → `f == 0.f`), which
    /// gives zero-length axes the half-open face check. On success the two
    /// endpoints are moved to the clipped parameters; `false` means the
    /// segment misses the bloated box entirely.
    pub fn clip(&self, source: &mut Vector3, extent: &mut Vector3, bloat: f32) -> bool {
        let mut s_min = 0.0f32;
        let mut e_max = 1.0f32;

        let delta = *extent - *source;

        // check the x faces
        if delta.x != 0.0 {
            // find the parameter along the line as it crosses these faces
            let mt_x = (self.min.x - bloat - source.x) / delta.x;
            let mt_up_x = (self.max.x + bloat - source.x) / delta.x;

            // see if it moves in the min or max,
            //  (depends on line dir in this axis)
            if delta.x > 0.0 {
                if mt_x > s_min {
                    s_min = mt_x;
                }
                if mt_up_x < e_max {
                    e_max = mt_up_x;
                }
            } else {
                if mt_up_x > s_min {
                    s_min = mt_up_x;
                }
                if mt_x < e_max {
                    e_max = mt_x;
                }
            }
        } else if source.x < self.min.x - bloat || source.x >= self.max.x + bloat {
            return false;
        }

        // check the y faces
        if delta.y != 0.0 {
            let mt_y = (self.min.y - bloat - source.y) / delta.y;
            let mt_up_y = (self.max.y + bloat - source.y) / delta.y;
            if delta.y > 0.0 {
                if mt_y > s_min {
                    s_min = mt_y;
                }
                if mt_up_y < e_max {
                    e_max = mt_up_y;
                }
            } else {
                if mt_up_y > s_min {
                    s_min = mt_up_y;
                }
                if mt_y < e_max {
                    e_max = mt_y;
                }
            }
        } else if source.y < self.min.y - bloat || source.y >= self.max.y + bloat {
            return false;
        }

        // check the z faces
        if delta.z != 0.0 {
            let mt_z = (self.min.z - bloat - source.z) / delta.z;
            let mt_up_z = (self.max.z + bloat - source.z) / delta.z;
            if delta.z > 0.0 {
                if mt_z > s_min {
                    s_min = mt_z;
                }
                if mt_up_z < e_max {
                    e_max = mt_up_z;
                }
            } else {
                if mt_up_z > s_min {
                    s_min = mt_up_z;
                }
                if mt_z < e_max {
                    e_max = mt_z;
                }
            }
        } else if source.z < self.min.z - bloat || source.z >= self.max.z + bloat {
            return false;
        }

        // get out if it's not inside
        if s_min >= e_max {
            return false;
        }

        // otherwise set the points the the calculated parameters
        // (reference: `source + eMax * delta` — float*vector, bitwise the
        // same as the vector*scale form used here)
        *extent = *source + delta * e_max;
        *source = *source + delta * s_min;

        // and that's it
        true
    }

    /// Efficient eight-corner transform (`boundbox.cpp:73-118`): transform
    /// the min corner, then accumulate the axis-aligned extents row by row
    /// with sign handling.
    pub fn transform_by(&mut self, m: &Matrix) {
        let min = m.apply_point(&self.min);
        let mut new_min = min;
        let mut new_max = min;

        let size = self.width();
        for axis in 0..3 {
            let dir = m.apply_to_unit_axis_vector(axis);
            let extent = size.get(axis);
            for comp in 0..3 {
                let scaled = dir.get(comp) * extent;
                if scaled > 0.0 {
                    new_max.set(comp, new_max.get(comp) + scaled);
                } else {
                    new_min.set(comp, new_min.get(comp) + scaled);
                }
            }
        }
        self.min = new_min;
        self.max = new_max;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_inside_out() {
        let b = BoundBox::default();
        assert!(b.inside_out());
        assert_eq!(b.min.x, f32::MAX);
        assert_eq!(b.max.x, f32::MIN);
    }

    #[test]
    fn add_bounds_snaps_inside_out_box() {
        let mut b = BoundBox::default();
        b.add_bounds(&Vector3::new(1.0, 2.0, 3.0));
        assert_eq!(b.min, Vector3::new(1.0, 2.0, 3.0));
        assert_eq!(b.max, Vector3::new(1.0, 2.0, 3.0));
    }

    #[test]
    fn add_bounds_grows_min_max() {
        let mut b = BoundBox::new(Vector3::ZERO, Vector3::new(10.0, 10.0, 10.0));
        b.add_bounds(&Vector3::new(-5.0, 11.0, 5.0));
        assert_eq!(b.min, Vector3::new(-5.0, 0.0, 0.0));
        assert_eq!(b.max, Vector3::new(10.0, 11.0, 10.0));
    }

    #[test]
    fn add_bound_box_merges_and_snaps_like_the_reference() {
        // an inside-out source is skipped entirely (`boundbox.ipp:213-214`)
        let mut b = BoundBox::new(Vector3::ZERO, Vector3::new(10.0, 10.0, 10.0));
        b.add_bound_box(&BoundBox::default());
        assert_eq!(b.min, Vector3::ZERO);
        assert_eq!(b.max, Vector3::new(10.0, 10.0, 10.0));
        // an inside-out self snaps to the source (`boundbox.ipp:216-221`)
        let mut c = BoundBox::default();
        c.add_bound_box(&BoundBox::new(
            Vector3::new(1.0, 2.0, 3.0),
            Vector3::new(4.0, 5.0, 6.0),
        ));
        assert_eq!(c.min, Vector3::new(1.0, 2.0, 3.0));
        assert_eq!(c.max, Vector3::new(4.0, 5.0, 6.0));
        // otherwise per-component min/max
        let mut d = BoundBox::new(Vector3::ZERO, Vector3::new(10.0, 10.0, 10.0));
        d.add_bound_box(&BoundBox::new(
            Vector3::new(-1.0, 11.0, 5.0),
            Vector3::new(2.0, 12.0, 6.0),
        ));
        assert_eq!(d.min, Vector3::new(-1.0, 0.0, 0.0));
        assert_eq!(d.max, Vector3::new(10.0, 12.0, 10.0));
    }

    #[test]
    fn point_containment_is_half_open() {
        let b = BoundBox::new(Vector3::ZERO, Vector3::new(10.0, 10.0, 10.0));
        assert!(b.contains(&Vector3::ZERO), "min corner is inside");
        assert!(
            !b.contains(&Vector3::new(10.0, 0.0, 0.0)),
            "max corner is outside"
        );
        assert!(b.contains(&Vector3::new(9.999, 0.0, 0.0)));
        assert!(!b.contains(&Vector3::new(-0.001, 0.0, 0.0)));
    }

    #[test]
    fn box_overlap_is_closed_interval() {
        let a = BoundBox::new(Vector3::ZERO, Vector3::new(10.0, 10.0, 10.0));
        let touching = BoundBox::new(Vector3::new(10.0, 0.0, 0.0), Vector3::new(20.0, 10.0, 10.0));
        assert!(
            a.intersects(&touching),
            "closed interval includes touching faces"
        );

        let apart = BoundBox::new(
            Vector3::new(10.001, 0.0, 0.0),
            Vector3::new(20.0, 10.0, 10.0),
        );
        assert!(!a.intersects(&apart));
    }

    #[test]
    fn centre_and_width() {
        let b = BoundBox::new(Vector3::new(-1.0, -2.0, -3.0), Vector3::new(1.0, 2.0, 3.0));
        assert_eq!(b.centre(), Vector3::ZERO);
        assert_eq!(b.width(), Vector3::new(2.0, 4.0, 6.0));
    }

    #[test]
    fn transform_by_translation_moves_the_box() {
        let mut b = BoundBox::new(Vector3::new(0.0, 0.0, 0.0), Vector3::new(1.0, 2.0, 3.0));
        let mut m = Matrix::zero();
        m.set_translate(5.0, 6.0, 7.0);
        b.transform_by(&m);
        assert_eq!(b.min, Vector3::new(5.0, 6.0, 7.0));
        assert_eq!(b.max, Vector3::new(6.0, 8.0, 10.0));
    }

    #[test]
    fn transform_by_rotation_grows_the_box() {
        let mut b = BoundBox::new(Vector3::new(-1.0, -1.0, 0.0), Vector3::new(1.0, 1.0, 0.0));
        let mut m = Matrix::zero();
        m.set_rotate_z(std::f32::consts::FRAC_PI_2);
        b.transform_by(&m);
        // A 90° rotation of a square about its centre keeps the footprint but
        // must never shrink it.
        assert!((b.width().x - 2.0).abs() < 1e-5);
        assert!((b.width().y - 2.0).abs() < 1e-5);
        assert!(b.intersects(&BoundBox::new(
            Vector3::new(-1.0, -1.0, 0.0),
            Vector3::new(1.0, 1.0, 0.0)
        )));
    }

    #[test]
    fn clip_passes_and_rewrites_a_segment_through_the_box() {
        let b = BoundBox::new(Vector3::new(-2.0, -3.0, -2.0), Vector3::new(2.0, 1.0, 2.0));
        // A segment crossing the box end to end: the exit lands on the
        // max face, the entry on the min face.
        let mut s = Vector3::new(-5.0, -1.0, 0.0);
        let mut e = Vector3::new(5.0, -1.0, 0.0);
        assert!(b.clip(&mut s, &mut e, 0.0));
        assert_eq!(s, Vector3::new(-2.0, -1.0, 0.0));
        assert_eq!(e, Vector3::new(2.0, -1.0, 0.0));

        // A zero-bloat miss is rejected.
        let mut s = Vector3::new(-5.0, 1.5, 0.0);
        let mut e = Vector3::new(5.0, 1.5, 0.0);
        assert!(!b.clip(&mut s, &mut e, 0.0));
    }

    #[test]
    fn clip_bloats_the_faces() {
        let b = BoundBox::new(Vector3::new(-2.0, -3.0, -2.0), Vector3::new(2.0, 1.0, 2.0));
        // y is a zero-delta axis here: y = 1.5 sits inside the bloated slab
        // [-4, 2], so the half-open face check accepts it. The clip happens
        // on x, where the 1.0-bloat moves the faces from ±2 to ±3.
        let mut s = Vector3::new(-5.0, 1.5, 0.0);
        let mut e = Vector3::new(5.0, 1.5, 0.0);
        assert!(b.clip(&mut s, &mut e, 1.0));
        assert_eq!(s, Vector3::new(-3.0, 1.5, 0.0));
        assert_eq!(e, Vector3::new(3.0, 1.5, 0.0));
    }

    #[test]
    fn clip_zero_delta_axis_uses_the_half_open_face_check() {
        let b = BoundBox::new(Vector3::new(-2.0, -3.0, -2.0), Vector3::new(2.0, 1.0, 2.0));
        // delta.x == 0 exactly with x inside the bloated slab: the x axis
        // accepts, the clip still runs on y/z.
        let mut s = Vector3::new(0.0, -5.0, 0.0);
        let mut e = Vector3::new(0.0, 5.0, 0.0);
        assert!(b.clip(&mut s, &mut e, 0.0));
        assert_eq!(s.x, 0.0);
        assert_eq!(e.x, 0.0);
        assert_eq!(s.y, -3.0);
        assert_eq!(e.y, 1.0);

        // x on the min face - bloat (exclusive) is rejected.
        let mut s = Vector3::new(-2.1, 0.0, 0.0);
        let mut e = Vector3::new(-2.1, 0.0, 0.5);
        assert!(!b.clip(&mut s, &mut e, 0.0));
        // x exactly on min - bloat is accepted (half-open at min).
        let mut s = Vector3::new(-2.0, 0.0, 0.0);
        let mut e = Vector3::new(-2.0, 0.0, 0.5);
        assert!(b.clip(&mut s, &mut e, 0.0));
        // x exactly on max + bloat is rejected (half-open at max).
        let mut s = Vector3::new(2.0, 0.0, 0.0);
        let mut e = Vector3::new(2.0, 0.0, 0.5);
        assert!(!b.clip(&mut s, &mut e, 0.0));
    }

    #[test]
    fn clip_rejects_when_the_entry_passes_the_exit() {
        // Both endpoints beyond the box on one axis: the entry parameter
        // s_min lands past the exit parameter e_max.
        let b = BoundBox::new(Vector3::new(-1.0, -1.0, -1.0), Vector3::new(1.0, 1.0, 1.0));
        let mut s = Vector3::new(3.0, 0.0, 0.0);
        let mut e = Vector3::new(5.0, 0.0, 0.0);
        assert!(!b.clip(&mut s, &mut e, 0.0));
        // Endpoints unchanged on rejection.
        assert_eq!(s, Vector3::new(3.0, 0.0, 0.0));
        assert_eq!(e, Vector3::new(5.0, 0.0, 0.0));
    }

    #[test]
    fn clip_walks_a_negative_direction_axis() {
        let b = BoundBox::new(Vector3::new(-2.0, -2.0, -2.0), Vector3::new(2.0, 2.0, 2.0));
        // z sweeps from +5 down to -5: entry at the max face, exit at the
        // min face (the swapped branch).
        let mut s = Vector3::new(0.0, 0.0, 5.0);
        let mut e = Vector3::new(0.0, 0.0, -5.0);
        assert!(b.clip(&mut s, &mut e, 0.0));
        assert_eq!(s, Vector3::new(0.0, 0.0, 2.0));
        assert_eq!(e, Vector3::new(0.0, 0.0, -2.0));
    }
}
