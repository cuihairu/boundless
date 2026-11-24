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
