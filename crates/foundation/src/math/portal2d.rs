//! BW-10-01-05 convex portal in 2D, following the reference in
//! `lib/math/portal2d.{hpp,cpp,ipp}`.
//!
//! A `Portal2D` is a vector of points forming a convex polygon. It is the
//! scratch type the hull tree uses to clip a plane's outline against the
//! other planes of a border (`HullTree::addPlane`). The reference's
//! reference-count field is never read anywhere in the tree (`refs` is only
//! ever assigned through its setter, and `Portal3D` takes a plain
//! `Portal2D&`), so it is left out here; that is a registered divergence,
//! not a behaviour change.

use super::vector::Vector2;

/// The reference `PORTAL_EPSILON` (`portal2d.cpp:66`).
const PORTAL_EPSILON: f32 = 1.0e-10;

/// `Portal2D` (`portal2d.hpp:14-40`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Portal2D {
    points: Vec<Vector2>,
}

impl Portal2D {
    /// `setDefaults` (`portal2d.cpp:73-85`): the clip-space quad.
    pub fn set_defaults(&mut self) {
        self.points.clear();
        self.points.push(Vector2::new(-1.0, 1.0));
        self.points.push(Vector2::new(1.0, 1.0));
        self.points.push(Vector2::new(1.0, -1.0));
        self.points.push(Vector2::new(-1.0, -1.0));
    }

    /// `addPoint` (`portal2d.ipp:15-18`).
    pub fn add_point(&mut self, v: Vector2) {
        self.points.push(v);
    }

    /// `erasePoints` (`portal2d.hpp:22`).
    pub fn erase_points(&mut self) {
        self.points.clear();
    }

    /// `points()` (`portal2d.hpp:35`).
    pub fn points(&self) -> &[Vector2] {
        &self.points
    }

    /// `contains` (`portal2d.cpp:255-274`): whether the point is on the
    /// inner side of every edge.
    pub fn contains(&self, v: &Vector2) -> bool {
        if self.points.is_empty() {
            return false;
        }
        let mut pt_a = *self.points.last().unwrap();
        for pt_b in self.points.iter() {
            let seg = *pt_b - pt_a;
            let nor = Vector2::new(seg.y, -seg.x);
            if nor.dot_product(v) < nor.dot_product(&pt_a) {
                return false;
            }
            pt_a = *pt_b;
        }
        true
    }

    /// `combine` (`portal2d.cpp:89-183`): clip `pt2` by every edge of
    /// `pt1`, leaving the intersection in `self` and returning whether it
    /// has at least three points. The reference wraps `pt1`'s first point
    /// onto the end, walks the edges, and unwraps before returning; this
    /// port indexes the wrap instead so `pt1` is never mutated (an empty
    /// `pt1` clips nothing instead of reading past the vector).
    pub fn combine(&mut self, pt1: &Portal2D, pt2: &Portal2D) -> bool {
        // start with pt2's points
        self.points.clear();
        self.points.extend_from_slice(pt2.points());

        // walk pt1's edges, closing the loop back to the first point
        let pt1_len = pt1.points.len();
        if pt1_len == 0 {
            return self.points.len() >= 3;
        }
        for i in 0..pt1_len - 1 {
            // the reference would read `points_.front()` off an emptied
            // clip set; the port stops and reports the (sub-3) result
            if self.points.is_empty() {
                return false;
            }
            let p_i = pt1.points[i];
            let n2 = pt1.points[i + 1] - p_i;
            let ls = n2.length_squared();
            // don't try to clip against really small edges
            if ls > -PORTAL_EPSILON && ls < PORTAL_EPSILON {
                continue;
            }
            // the edge's inner half-plane
            let n = Vector2::new(n2.y, -n2.x);
            let d = n.dot_product(&p_i);

            let mut ins: Vec<u8> = self
                .points
                .iter()
                .map(|p| if n.dot_product(p) >= d { 1 } else { 0 })
                .collect();
            // close the loop for the inside walk and the clip set too
            // (`portal2d.cpp:119-124`)
            let mut pts = self.points.clone();
            pts.push(pts[0]);
            ins.push(ins[0]);

            // find the crossing points and insert them
            let mut j = 0usize;
            while j + 1 < pts.len() {
                if ins[j] != ins[j + 1] {
                    let v = pts[j];
                    let dv = pts[j + 1] - v;
                    let t = clamp01((d - n.dot_product(&v)) / n.dot_product(&dv));
                    let v = v + dv * t;
                    j += 1;
                    pts.insert(j, v);
                    ins.insert(j, 1);
                }
                j += 1;
            }

            // unwrap again
            pts.pop();
            ins.pop();

            // drop the points outside the edge
            let mut kept: Vec<Vector2> = Vec::with_capacity(pts.len());
            for (point, &is_inside) in pts.iter().zip(ins.iter()) {
                if is_inside != 0 {
                    kept.push(*point);
                }
            }
            self.points = kept;
        }

        self.points.len() >= 3
    }
}

/// `Math::clamp( 0.f, t, 1.f )` (`mathdef.hpp`, `portal2d.cpp:159`).
fn clamp01(t: f32) -> f32 {
    super::vector::clamp(0.0, t, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    /// The reference's winding (`setDefaults` walks (-1,1),(1,1),(1,-1),
    /// (-1,-1)): the clipper's `n = (seg.y, -seg.x)` inner half-plane only
    /// holds for that clockwise (y-up) order.
    fn cw_box(min_x: f32, min_y: f32, max_x: f32, max_y: f32) -> Portal2D {
        let mut p = Portal2D::default();
        for corner in [
            Vector2::new(min_x, min_y),
            Vector2::new(min_x, max_y),
            Vector2::new(max_x, max_y),
            Vector2::new(max_x, min_y),
        ] {
            p.add_point(corner);
        }
        p
    }

    #[test]
    fn set_defaults_is_the_clip_space_quad() {
        let mut p = Portal2D::default();
        p.set_defaults();
        assert_eq!(
            p.points(),
            &[
                Vector2::new(-1.0, 1.0),
                Vector2::new(1.0, 1.0),
                Vector2::new(1.0, -1.0),
                Vector2::new(-1.0, -1.0),
            ]
        );
    }

    #[test]
    fn add_point_appends_in_order() {
        let mut p = Portal2D::default();
        p.add_point(Vector2::new(1.0, 2.0));
        p.add_point(Vector2::new(3.0, 4.0));
        assert_eq!(
            p.points(),
            &[Vector2::new(1.0, 2.0), Vector2::new(3.0, 4.0)]
        );
    }

    #[test]
    fn contains_checks_every_edge() {
        let p = cw_box(0.0, 0.0, 2.0, 2.0);
        assert!(p.contains(&Vector2::new(1.0, 1.0)));
        assert!(!p.contains(&Vector2::new(3.0, 1.0)));
        // a point exactly on an edge counts as inside (>=)
        assert!(p.contains(&Vector2::new(2.0, 1.0)));
    }

    #[test]
    fn an_empty_portal_contains_nothing() {
        let p = Portal2D::default();
        assert!(!p.contains(&Vector2::ZERO));
    }

    #[test]
    fn erase_points_clears_the_list() {
        let mut p = cw_box(0.0, 0.0, 1.0, 1.0);
        p.erase_points();
        assert!(p.points().is_empty());
        assert!(!p.contains(&Vector2::ZERO));
    }

    /// The reference reads `pt1->points_.front()` off an empty portal; the
    /// port's guard clips nothing and reports `pt2`'s own count instead.
    #[test]
    fn combining_with_an_empty_clipper_clips_nothing() {
        let mut out = Portal2D::default();
        let empty = Portal2D::default();
        let b = cw_box(1.0, 1.0, 3.0, 3.0);
        assert!(out.combine(&empty, &b));
        assert_eq!(out.points(), b.points());
        let mut single = Portal2D::default();
        single.add_point(Vector2::new(1.0, 2.0));
        assert!(!out.combine(&empty, &single));
    }

    #[test]
    fn combining_two_squares_gives_their_overlap() {
        let mut out = Portal2D::default();
        let a = cw_box(0.0, 0.0, 2.0, 2.0);
        let b = cw_box(1.0, 1.0, 3.0, 3.0);
        assert!(out.combine(&a, &b));
        // the overlap of [0,2]^2 and [1,3]^2 is [1,2]^2: four points, all
        // of them inside it
        assert_eq!(out.points().len(), 4);
        for p in out.points() {
            assert!(p.x >= 1.0 - 1e-5 && p.x <= 2.0 + 1e-5);
            assert!(p.y >= 1.0 - 1e-5 && p.y <= 2.0 + 1e-5);
        }
    }

    #[test]
    fn combining_disjoint_polygons_fails() {
        let mut out = Portal2D::default();
        let a = cw_box(0.0, 0.0, 1.0, 1.0);
        let b = cw_box(100.0, 100.0, 101.0, 101.0);
        assert!(!out.combine(&a, &b));
    }

    #[test]
    fn combining_a_tiny_edge_is_skipped() {
        // A triangle plus a duplicated last point: that edge has zero
        // length and is skipped, so the clip equals the triangle's own.
        let mut a = Portal2D::default();
        let mut b = Portal2D::default();
        for p in [
            Vector2::new(0.0, 0.0),
            Vector2::new(1.0, 2.0),
            Vector2::new(2.0, 0.0),
        ] {
            a.add_point(p);
            b.add_point(p);
        }
        b.add_point(Vector2::new(2.0, 0.0));

        let mut plain = Portal2D::default();
        assert!(plain.combine(&a, &a));
        let mut with_duplicate = Portal2D::default();
        assert!(with_duplicate.combine(&b, &a));
        assert_eq!(plain.points().len(), 3);
        assert_eq!(with_duplicate.points(), plain.points());
    }
}
