//! BW-13-05-03 the Graham scan convex hull (`lib/math/graham_scan.hpp`).
//!
//! Finds the convex hull of a set of coplanar 3D points: the plane is
//! taken through the first three of them, the points are projected into
//! its parameterised 2D basis, scanned, and written back unprojected.

use super::plane::PlaneEq;
use super::vector::Vector3;

/// `GrahamScan(pts)` (`graham_scan.hpp:21-29`) — the constructor is the
/// whole API: with more than three points it builds the plane through
/// the first three and replaces `pts` with the hull (in stack order,
/// bottom to top).
pub fn graham_scan(pts: &mut Vec<Vector3>) {
    if pts.len() > 3 {
        let mut plane = PlaneEq::new(Vector3::ZERO, 0.0);
        plane.init(&pts[0], &pts[1], &pts[2]);
        make_convex_hull(pts, &plane);
    }
}

/// `isLeft(pts, pt)` (`graham_scan.hpp:85-89`): is `pt` left of the line
/// `pts[0] -> pts[1]`?
fn is_left(line0: &Vector3, line1: &Vector3, pt: &Vector3) -> f32 {
    (line1.x - line0.x) * (pt.y - line0.y) - (pt.x - line0.x) * (line1.y - line0.y)
}

/// `makeConvexHull` (`graham_scan.hpp:119-191`).
fn make_convex_hull(pts: &mut Vec<Vector3>, plane: &PlaneEq) {
    // 0. early out, any triangle would already be a convex hull.
    if pts.len() <= 3 {
        return;
    }

    // From here in, we work in 2D space, with all points projected onto
    // the plane (GrahamScan only works on 2D hulls)
    for p in pts.iter_mut() {
        let prj = plane.project(p);
        p.x = prj.x;
        p.y = prj.y;
    }

    // 1. First find the fulcrum point, with the lowest Y projected onto
    // the plane.
    pts.sort_unstable_by(|a, b| {
        if (a.y - b.y).abs() < f32::EPSILON {
            a.x.partial_cmp(&b.x).expect("no NaN in hull input")
        } else {
            a.y.partial_cmp(&b.y).expect("no NaN in hull input")
        }
    });

    // 2. Sort the points by angle between them and the fulcrum point.
    // The reference comparator (`(tanA.x*tanB.y) > (tanB.x*tanA.y)`) puts
    // `a` first when its angle is the smaller one, and is not a strict
    // weak ordering — `std::sort`'s order for equal angles is
    // unspecified, which `sort_unstable_by` matches.
    let fulcrum = pts[0];
    pts[1..].sort_unstable_by(|a, b| {
        let lhs = (a.x - fulcrum.x) * (b.y - fulcrum.y);
        let rhs = (b.x - fulcrum.x) * (a.y - fulcrum.y);
        rhs.partial_cmp(&lhs).expect("no NaN in hull input")
    });

    // 3. Now go through the sorted list, discarding only left hand turns
    let mut stack: Vec<Vector3> = Vec::new();
    stack.push(pts[0]);
    stack.push(pts[1]);

    let mut cp;
    let mut i = 2;
    while i < pts.len() {
        cp = is_left(&stack[stack.len() - 2], &stack[stack.len() - 1], &pts[i]);
        if cp > 0.0 {
            stack.push(pts[i]);
        } else {
            // The reference `do { pop; if (size > 2) recompute } while
            // (cp <= 0 && size > 2)`: once the stack is down to two
            // points `cp` stays stale, and the size check ends the loop.
            loop {
                stack.pop();
                if stack.len() > 2 {
                    cp = is_left(&stack[stack.len() - 2], &stack[stack.len() - 1], &pts[i]);
                }
                if !(cp <= 0.0 && stack.len() > 2) {
                    break;
                }
            }
            stack.push(pts[i]);
        }
        i += 1;
    }

    // Finally, check the last point is inside the hull. (Reached only
    // when the closing edge to the fulcrum turns non-left, which for a
    // stable sort on ties cannot happen — kept verbatim like the
    // reference.)
    if is_left(&stack[stack.len() - 2], &stack[stack.len() - 1], &stack[0]) <= 0.0 {
        stack.pop();
    }

    // Now, put the results back in the list: the reference pops the
    // stack into `pts_` back to front, so the vector ends up in stack
    // order, bottom to top.
    *pts = stack;

    // And return to 3D space.
    for p in pts.iter_mut() {
        *p = plane.param(&super::vector::Vector2::new(p.x, p.y));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A square with an interior point: the hull keeps the four corners
    /// in counter-clockwise stack order, the interior point is dropped.
    #[test]
    fn square_hull_drops_the_interior_point() {
        let mut pts = vec![
            Vector3::new(0.0, 0.0, 0.0),
            Vector3::new(2.0, 0.0, 0.0),
            Vector3::new(2.0, 2.0, 0.0),
            Vector3::new(0.0, 2.0, 0.0),
            Vector3::new(0.5, 1.5, 0.0),
        ];
        graham_scan(&mut pts);
        assert_eq!(
            pts,
            vec![
                Vector3::new(0.0, 0.0, 0.0),
                Vector3::new(2.0, 0.0, 0.0),
                Vector3::new(2.0, 2.0, 0.0),
                Vector3::new(0.0, 2.0, 0.0),
            ]
        );
    }

    /// An interior point near the closing edge drains the stack down to
    /// two points: the pop loop exits on the stale `cp` via the size
    /// check, exactly like the reference.
    #[test]
    fn interior_point_pops_down_to_two_and_restarts() {
        let mut pts = vec![
            Vector3::new(0.0, 0.0, 0.0),
            Vector3::new(3.0, 0.0, 0.0),
            Vector3::new(1.0, 1.0, 0.0),
            Vector3::new(0.0, 3.0, 0.0),
        ];
        graham_scan(&mut pts);
        assert_eq!(
            pts,
            vec![
                Vector3::new(0.0, 0.0, 0.0),
                Vector3::new(3.0, 0.0, 0.0),
                Vector3::new(0.0, 3.0, 0.0),
            ]
        );
    }

    /// A point exactly on a hull edge is non-left (`cp == 0`) and gets
    /// popped; two points inside the same edge drain in one pass.
    #[test]
    fn points_on_the_hull_edge_are_dropped() {
        let mut pts = vec![
            Vector3::new(0.0, 0.0, 0.0),
            Vector3::new(4.0, 0.0, 0.0),
            Vector3::new(3.0, 1.0, 0.0),
            Vector3::new(0.0, 4.0, 0.0),
            Vector3::new(0.0, 2.0, 0.0),
        ];
        graham_scan(&mut pts);
        assert_eq!(
            pts,
            vec![
                Vector3::new(0.0, 0.0, 0.0),
                Vector3::new(4.0, 0.0, 0.0),
                Vector3::new(0.0, 4.0, 0.0),
            ]
        );
    }

    /// Three points or fewer are left untouched — no plane is built and
    /// no sort runs.
    #[test]
    fn three_points_are_left_untouched() {
        let mut pts = vec![
            Vector3::new(1.0, 0.0, 0.0),
            Vector3::new(0.0, 1.0, 0.0),
            Vector3::new(0.0, 0.0, 1.0),
        ];
        graham_scan(&mut pts);
        assert_eq!(
            pts,
            vec![
                Vector3::new(1.0, 0.0, 0.0),
                Vector3::new(0.0, 1.0, 0.0),
                Vector3::new(0.0, 0.0, 1.0),
            ]
        );
    }

    /// A plane off the origin (`z = 1`): the projection and the
    /// unprojection round-trip through the plane's `d`.
    #[test]
    fn hull_on_an_offset_plane_round_trips() {
        let mut pts = vec![
            Vector3::new(0.0, 0.0, 1.0),
            Vector3::new(2.0, 0.0, 1.0),
            Vector3::new(2.0, 2.0, 1.0),
            Vector3::new(0.0, 2.0, 1.0),
            Vector3::new(0.5, 1.5, 1.0),
        ];
        graham_scan(&mut pts);
        assert_eq!(
            pts,
            vec![
                Vector3::new(0.0, 0.0, 1.0),
                Vector3::new(2.0, 0.0, 1.0),
                Vector3::new(2.0, 2.0, 1.0),
                Vector3::new(0.0, 2.0, 1.0),
            ]
        );
    }

    /// A hull in the x/z plane: the plane through the first three input
    /// points winds to the normal (0,−1,0), whose projected basis is
    /// `(z, −x)` — so the fulcrum (lowest projected y) is `(2,0,0)` and
    /// the hull starts there. The interior point is still dropped.
    #[test]
    fn hull_in_the_xz_plane() {
        let mut pts = vec![
            Vector3::new(0.0, 0.0, 0.0),
            Vector3::new(2.0, 0.0, 0.0),
            Vector3::new(2.0, 0.0, 2.0),
            Vector3::new(0.0, 0.0, 2.0),
            Vector3::new(0.5, 0.0, 1.5),
        ];
        graham_scan(&mut pts);
        assert_eq!(
            pts,
            vec![
                Vector3::new(2.0, 0.0, 0.0),
                Vector3::new(2.0, 0.0, 2.0),
                Vector3::new(0.0, 0.0, 2.0),
                Vector3::new(0.0, 0.0, 0.0),
            ]
        );
    }
}
