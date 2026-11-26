//! BW-01-01-05 axis-aligned bounding boxes (`lib/math/boundbox.hpp/.cpp`).

use super::matrix::Matrix;
use super::vector::Vector3;

/// `min` starts at `FLT_MAX`, `max` at `-FLT_MAX`: the default box is
/// inside-out (`boundbox.ipp:10-14`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum RayIntersectionType {
    NoIntersection,
    OriginInside,
    Intersection,
}

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

    /// `(min + max) / 2.f` (`boundbox.ipp:323-328`).
    pub fn centre(&self) -> Vector3 {
        (self.min + self.max) / 2.0
    }

    pub fn width(&self) -> Vector3 {
        self.max - self.min
    }

    /// Assign both corners directly (`boundbox.ipp:104-108`) — no
    /// normalisation, unlike `add_bounds`.
    pub fn set_bounds(&mut self, min: Vector3, max: Vector3) {
        self.min = min;
        self.max = max;
    }

    /// Distance from the box to a point: per-axis clamping to the nearest
    /// face, then the length of the accumulated offsets; a contained point
    /// reads zero (`boundbox.cpp:397-417`).
    pub fn distance(&self, point: &Vector3) -> f32 {
        let mut offset = Vector3::new(0.0, 0.0, 0.0);

        if point.x > self.max.x {
            offset.x = point.x - self.max.x;
        } else if point.x < self.min.x {
            offset.x = point.x - self.min.x;
        }

        if point.y > self.max.y {
            offset.y = point.y - self.max.y;
        } else if point.y < self.min.y {
            offset.y = point.y - self.min.y;
        }

        if point.z > self.max.z {
            offset.z = point.z - self.max.z;
        } else if point.z < self.min.z {
            offset.z = point.z - self.min.z;
        }

        offset.length()
    }

    /// Whether the box grown by `bias` on every axis contains the point —
    /// the light volumes' sphere probe (`boundbox.ipp:313-321`), whose
    /// second parameter is a bias, not a radius.
    pub fn intersects_point(&self, v: &Vector3, bias: f32) -> bool {
        v.x >= self.min.x - bias
            && v.y >= self.min.y - bias
            && v.z >= self.min.z - bias
            && v.x < self.max.x + bias
            && v.y < self.max.y + bias
            && v.z < self.max.z + bias
    }

    /// Efficient eight-corner transform (`boundbox.cpp:73-118`): transform
    /// the min corner, then accumulate the axis-aligned extents row by row
    /// with sign handling.
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
    pub fn intersects_ray(&self, origin: &Vector3, dir: &Vector3) -> bool {
        let bounds = [self.min, self.max];
        for bound in &bounds {
            for axis in 0..3 {
                if dir.get(axis) != 0.0 {
                    let t = (bound.get(axis) - origin.get(axis)) / dir.get(axis);

                    if t > 0.0 {
                        let pt_on_plane = *origin + *dir * t;

                        let next_axis = (axis + 1) % 3;
                        let prev_axis = (axis + 2) % 3;

                        if self.min.get(next_axis) < pt_on_plane.get(next_axis)
                            && pt_on_plane.get(next_axis) < self.max.get(next_axis)
                            && self.min.get(prev_axis) < pt_on_plane.get(prev_axis)
                            && pt_on_plane.get(prev_axis) < self.max.get(prev_axis)
                        {
                            return true;
                        }
                    }
                }
            }
        }

        false
    }
    pub fn intersects_ray_hit(
        &self,
        origin: &Vector3,
        dir: &Vector3,
        coord: &mut Vector3,
    ) -> RayIntersectionType {
        const RAYAABB_EPSILON: f32 = 0.00001;

        let mut is_inside = true;
        let min_bounds = self.min;
        let max_bounds = self.max;
        let mut max_t = Vector3::new(-1.0, -1.0, -1.0);

        // Find candidate planes.
        for i in 0..3 {
            if origin.get(i) < min_bounds.get(i) {
                coord.set(i, min_bounds.get(i));
                is_inside = false;

                // Calculate T distances to candidate planes
                if dir.get(i).to_bits() != 0 {
                    max_t.set(i, (min_bounds.get(i) - origin.get(i)) / dir.get(i));
                }
            } else if origin.get(i) > max_bounds.get(i) {
                coord.set(i, max_bounds.get(i));
                is_inside = false;

                if dir.get(i).to_bits() != 0 {
                    max_t.set(i, (max_bounds.get(i) - origin.get(i)) / dir.get(i));
                }
            }
        }

        // Ray origin inside bounding box
        if is_inside {
            *coord = *origin;
            return RayIntersectionType::OriginInside;
        }

        // Get smallest of the maxT's for final choice of intersection
        let mut which_plane = 0usize;
        if max_t.get(1) > max_t.get(which_plane) {
            which_plane = 1;
        }
        if max_t.get(2) > max_t.get(which_plane) {
            which_plane = 2;
        }

        // Check final candidate actually inside box
        if max_t.get(which_plane).to_bits() & 0x8000_0000 != 0 {
            return RayIntersectionType::NoIntersection;
        }

        for i in 0..3 {
            if i != which_plane {
                coord.set(i, origin.get(i) + max_t.get(which_plane) * dir.get(i));
                if coord.get(i) < min_bounds.get(i) - RAYAABB_EPSILON
                    || coord.get(i) > max_bounds.get(i) + RAYAABB_EPSILON
                {
                    return RayIntersectionType::NoIntersection;
                }
            }
        }
        RayIntersectionType::Intersection
    }
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
    fn set_bounds_assigns_both_corners() {
        let mut b = BoundBox::default();
        b.set_bounds(Vector3::new(1.0, 2.0, 3.0), Vector3::new(4.0, 5.0, 6.0));
        assert_eq!(b.min, Vector3::new(1.0, 2.0, 3.0));
        assert_eq!(b.max, Vector3::new(4.0, 5.0, 6.0));
    }

    #[test]
    fn distance_is_zero_for_a_contained_point() {
        let b = BoundBox::new(Vector3::new(-1.0, -1.0, -1.0), Vector3::new(1.0, 1.0, 1.0));
        assert_eq!(b.distance(&Vector3::ZERO), 0.0);
        // A point beyond a single face measures the shortest offset only.
        assert_eq!(b.distance(&Vector3::new(3.0, 0.0, 0.0)), 2.0);
        // A point past a corner is the euclidean offset.
        assert!(
            (b.distance(&Vector3::new(2.0, 2.0, 2.0)) - (3.0f32).sqrt()).abs() < 1e-5,
            "corner distance sums the three axis offsets"
        );
        // Points before min measure to the min faces.
        assert_eq!(b.distance(&Vector3::new(-3.0, -1.0, -1.0)), 2.0);
    }

    #[test]
    fn intersects_point_grows_the_box_by_the_bias() {
        let b = BoundBox::new(Vector3::ZERO, Vector3::new(10.0, 10.0, 10.0));
        // Zero bias: the half-open containment test.
        assert!(b.intersects_point(&Vector3::new(5.0, 5.0, 5.0), 0.0));
        assert!(!b.intersects_point(&Vector3::new(-1.0, 5.0, 5.0), 0.0));
        // A positive bias admits points within radius of the faces; the
        // grown box stays half-open (`12.0 < 10.0 + 2.0` is false).
        assert!(b.intersects_point(&Vector3::new(-1.0, 5.0, 5.0), 2.0));
        assert!(b.intersects_point(&Vector3::new(11.9, 5.0, 5.0), 2.0));
        assert!(!b.intersects_point(&Vector3::new(12.0, 5.0, 5.0), 2.0));
        assert!(!b.intersects_point(&Vector3::new(-3.0, 5.0, 5.0), 2.0));
    }
}
