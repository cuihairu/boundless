//! BW-10-03-06: the convex polygon the sweep's brute-force path clips with
//! prism planes (`lib/physics2/worldpoly.hpp/.cpp`). Only `chop` is ported
//! — `WorldPolygon::split` has no caller in the sweep face (registered).

use foundation::math::{PlaneEq, Vector3};

/// A `WorldPolygon` (`worldpoly.hpp:20-33`): a plain point list. The
/// reference constructs the sweep's working polygon as `WorldPolygon(8)`
/// — a capacity hint, not a bound.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct WorldPolygon {
    points: Vec<Vector3>,
}

/// `tooClose` (`worldpoly.cpp:31-37`): a millimetre.
fn too_close(p1: &Vector3, p2: &Vector3) -> bool {
    const MIN_DIST: f32 = 0.001;
    const MIN_DIST2: f32 = MIN_DIST * MIN_DIST;
    (*p1 - *p2).length_squared() <= MIN_DIST2
}

impl WorldPolygon {
    /// The sweep brute path seeds the polygon with a triangle
    /// (`worldtri.cpp:419-423`).
    pub fn from_triangle(v0: Vector3, v1: Vector3, v2: Vector3) -> Self {
        Self {
            points: vec![v0, v1, v2],
        }
    }

    /// The surviving points (test view).
    pub fn points(&self) -> &[Vector3] {
        &self.points
    }

    /// `WorldPolygon::chop` (`worldpoly.cpp:159-221`): keep the part of
    /// the polygon in front of the plane (signed distance `>= 0`); cut
    /// points are interpolated on every side change, then points closer
    /// than a millimetre to their (wrap-around) successor are dropped.
    /// Returns whether at least three points survive.
    pub fn chop(&mut self, plane_eq: &PlaneEq) -> bool {
        // Early out and clear the poly if we have less than 3 verts.
        if self.points.len() < 3 {
            self.points.clear();
            return false;
        }

        // StaticArray<Vector3, 32> in the reference; a triangle chopped by
        // the five prism planes can never approach the 32-point cap.
        let mut new_points: Vec<Vector3> = Vec::new();

        // Calculate the first distance to the plane, as we recycle this.
        let mut last_dist = plane_eq.distance_to(&self.points[0]);

        // Iterate over all the points in the polygon.
        let len = self.points.len();
        for i in 0..len {
            // Grab the last point, if it's in front of the plane add it.
            let last_point = self.points[i];
            if last_dist >= 0.0 {
                new_points.push(last_point);
            }

            // Grab the current point and calculate the distance.
            let point = self.points[(i + 1) % len];
            let dist = plane_eq.distance_to(&point);

            // On a side change, create a new point at the intersection.
            if (dist >= 0.0 && last_dist < 0.0) || (dist < 0.0 && last_dist >= 0.0) {
                new_points
                    .push(last_point + (point - last_point) * (-last_dist / (dist - last_dist)));
            }
            last_dist = dist;
        }

        // Copy back, skipping points too close to their successor (the
        // wrap-around neighbour included).
        let mut kept: Vec<Vector3> = Vec::new();
        for i in 0..new_points.len() {
            if !too_close(&new_points[i], &new_points[(i + 1) % new_points.len()]) {
                kept.push(new_points[i]);
            }
        }
        self.points = kept;

        self.points.len() >= 3
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(x: f32, y: f32, z: f32) -> Vector3 {
        Vector3::new(x, y, z)
    }

    fn plane_z(d: f32) -> PlaneEq {
        PlaneEq::new(v(0.0, 0.0, 1.0), d)
    }

    #[test]
    fn chop_splits_a_triangle_into_a_quad() {
        let mut poly =
            WorldPolygon::from_triangle(v(0.0, 0.0, -1.0), v(2.0, 0.0, 1.0), v(0.0, 2.0, 1.0));
        // Keep the z >= 0 side: the first point is cut away, the other two
        // stay and two cut points appear.
        assert!(poly.chop(&plane_z(0.0)));
        assert_eq!(poly.points().len(), 4);
        // Cut point x for the edge (0,0,-1)->(2,0,1): ratio = -(-1)/(1-(-1)).
        assert_eq!(poly.points()[0], v(1.0, 0.0, 0.0));
        assert_eq!(poly.points()[1], v(2.0, 0.0, 1.0));
        assert_eq!(poly.points()[2], v(0.0, 2.0, 1.0));
        assert_eq!(poly.points()[3], v(0.0, 1.0, 0.0));
    }

    #[test]
    fn chop_clears_a_polygon_entirely_behind_the_plane() {
        let mut poly =
            WorldPolygon::from_triangle(v(0.0, 0.0, -3.0), v(1.0, 0.0, -1.0), v(0.0, 1.0, -2.0));
        assert!(!poly.chop(&plane_z(0.0)));
        assert!(poly.points().is_empty());
    }

    #[test]
    fn chop_keeps_a_polygon_fully_in_front_untouched() {
        let mut poly =
            WorldPolygon::from_triangle(v(0.0, 0.0, 1.0), v(2.0, 0.0, 3.0), v(0.0, 2.0, 2.0));
        let before = poly.clone();
        assert!(poly.chop(&plane_z(0.0)));
        assert_eq!(poly, before);
    }

    #[test]
    fn chop_rejects_and_clears_fewer_than_three_points() {
        let mut poly =
            WorldPolygon::from_triangle(v(0.0, 0.0, 1.0), v(1.0, 0.0, 1.0), v(0.0, 0.0, 1.0));
        poly.points.pop();
        assert_eq!(poly.points().len(), 2);
        assert!(!poly.chop(&plane_z(0.0)));
        assert!(poly.points().is_empty());
    }

    #[test]
    fn chop_drops_points_closer_than_a_millimetre_to_their_successor() {
        // The first two vertices are half a millimetre apart, so the first
        // is dropped by the successor check and only two points survive.
        let mut poly =
            WorldPolygon::from_triangle(v(0.0, 0.0, 1.0), v(0.0005, 0.0, 1.0), v(2.0, 0.0, 1.0));
        assert!(!poly.chop(&plane_z(0.0)));
        assert_eq!(poly.points().len(), 2);
        assert_eq!(poly.points()[0], v(0.0005, 0.0, 1.0));
        // Repeated chopping clears the degenerate remainder.
        assert!(!poly.chop(&plane_z(0.0)));
        assert!(poly.points().is_empty());
    }

    #[test]
    fn chop_cut_point_follows_the_reference_ratio() {
        // Asymmetric distances: ratio = -(-2)/(1-(-2)) = 2/3 along the edge.
        let mut poly =
            WorldPolygon::from_triangle(v(0.0, 0.0, -2.0), v(3.0, 0.0, 1.0), v(0.0, 3.0, 4.0));
        assert!(poly.chop(&plane_z(0.0)));
        assert_eq!(poly.points()[0], v(2.0, 0.0, 0.0));
    }

    #[test]
    fn too_close_uses_a_millimetre_squared_window() {
        assert!(too_close(&v(0.0, 0.0, 0.0), &v(0.0009, 0.0, 0.0)));
        assert!(!too_close(&v(0.0, 0.0, 0.0), &v(0.0011, 0.0, 0.0)));
    }
}
