//! BW-10-01-05 the convex-hull tree, a port of `lib/physics2/hulltree.cpp`.
//!
//! A `HullTree` stores convex hulls as a BSP over the hull borders' planes.
//! `add` walks the border planes pairwise, builds each plane's outline
//! polygon in 2D, and inserts it with a mark list that remembers which node
//! sides the new hull touches; `test_point` walks the dividers and returns
//! the tags whose hull contains the point. The reference keeps intrusive
//! pointers (`pFront_`/`pBack_`/`pNextMarked_`); the port keeps an arena of
//! nodes and a LIFO mark stack — same visit set and same per-node marking,
//! since the mark list only ever pushes each node once and the final tag
//! walk reads the marks node by node.
//!
//! Not ported (registered in `docs/review/bw10-01-chunk-load.md`): the
//! `Traversal` collision sweep and the `print`/`size` diagnostics.

use foundation::math::{LineEq, PlaneEq, Vector2, Vector3};

/// `HullBorder` (`hulltree.hpp:35`): the planes of one convex hull.
pub type HullBorder = Vec<PlaneEq>;

/// One BSP node. `tag_front`/`tag_back` hold the tags of hulls whose interior
/// reaches that side of the divider plane.
#[derive(Debug, Clone)]
struct Node {
    divider: PlaneEq,
    front: Option<usize>,
    back: Option<usize>,
    tag_front: Vec<usize>,
    tag_back: Vec<usize>,
    /// `marked_` bit layout (`hulltree.hpp:146-149`): `1` back, `2` front,
    /// `256` not-back, `512` not-front, `65536` new-in-this-hull.
    marked: i32,
}

impl Node {
    fn divider(plane: PlaneEq) -> Self {
        Self {
            divider: plane,
            front: None,
            back: None,
            tag_front: Vec::new(),
            tag_back: Vec::new(),
            marked: 2 | 65536,
        }
    }
}

/// The hull tree itself; node 0 is the root (a zero divider until the first
/// hull arrives, exactly like the reference default constructor).
#[derive(Debug, Clone, Default)]
pub struct HullTree {
    nodes: Vec<Node>,
}

impl HullTree {
    /// `HullTree()` (`hulltree.cpp:63-70`): a zero divider, no children.
    pub fn new() -> Self {
        Self {
            nodes: vec![Node {
                divider: PlaneEq::new(Vector3::ZERO, 0.0),
                front: None,
                back: None,
                tag_front: Vec::new(),
                tag_back: Vec::new(),
                marked: 0,
            }],
        }
    }

    /// `HullTree::add` (`hulltree.cpp:107-274`): add one hull. Every border
    /// plane gets its outline polygon built from the other planes' cut lines,
    /// is inserted via `add_plane`, and the mark stack is tagged at the end.
    pub fn add(&mut self, border: &HullBorder, tag: usize) {
        let mut marks: Vec<usize> = Vec::new();
        for (it, plane) in border.iter().enumerate() {
            // `Portal2D outline`: cut lines of every other plane in this
            // plane's own 2D basis (hulltree.cpp:116-184).
            let mut lines: Vec<LineEq> = Vec::new();
            for (bt, other) in border.iter().enumerate() {
                if bt == it {
                    continue;
                }
                let aline = plane.intersect(other);
                // Only add it if it's not parallel (=> facing plane, convex).
                if aline.normal() != &Vector2::ZERO {
                    lines.push(aline);
                }
            }

            let mut outline: Vec<Vector2> = Vec::new();
            for i in 0..lines.len() {
                let mut min_dist = -1_000_000.0f32;
                let mut max_dist = 1_000_000.0f32;
                for j in 0..lines.len() {
                    if j == i {
                        continue;
                    }
                    // Parallel: a facing line is ignored, a line in front of
                    // us is ignored, and otherwise we are cut off entirely
                    // (hulltree.cpp:143-155).
                    if lines[i].is_parallel(&lines[j]) {
                        if lines[i].normal().dot_product(lines[j].normal()) < 0.0 {
                            continue;
                        }
                        if lines[i].is_in_front_of(&lines[j].param(0.0)) {
                            continue;
                        }
                        min_dist = 1.0;
                        max_dist = -1.0;
                        break;
                    }
                    let i_dist = lines[i].intersect(&lines[j]);
                    if lines[i].is_min_cutter(&lines[j]) {
                        if min_dist < i_dist {
                            min_dist = i_dist;
                        }
                    } else if max_dist > i_dist {
                        max_dist = i_dist;
                    }
                }
                // There must be an edge between the two planes.
                if min_dist >= max_dist {
                    continue;
                }
                outline.push(lines[i].param(max_dist));
            }

            // `Portal3D`: lift the outline into the plane (hulltree.cpp:189,
            // 657-663).
            let outline3d: Vec<Vector3> = outline.iter().map(|p| plane.param(*p)).collect();
            self.add_plane(0, plane, &outline3d, &mut marks);
        }

        // Go through the marks and tag them (hulltree.cpp:241-266). The
        // reference walks its prepended list newest-first; the LIFO pop is
        // that same order.
        while let Some(idx) = marks.pop() {
            let node = &mut self.nodes[idx];
            if node.marked & 1 != 0 {
                node.tag_back.push(tag);
            }
            if node.marked & 2 != 0 {
                node.tag_front.push(tag);
            }
            node.marked = 0;
        }
    }

    /// `HullTree::addPlane` (`hulltree.cpp:280-467`): insert one plane's
    /// outline, descending past dividers the outline crosses and marking the
    /// node sides the hull touches.
    fn add_plane(
        &mut self,
        idx: usize,
        plane: &PlaneEq,
        outline: &[Vector3],
        marks: &mut Vec<usize>,
    ) {
        let mut any_side = [false, false];
        if self.nodes[idx].divider.normal() != &Vector3::ZERO {
            for point in outline {
                let d = self.nodes[idx].divider.distance_to(point);
                if d > 0.01 {
                    any_side[1] = true;
                } else if d < -0.01 {
                    any_side[0] = true;
                }
            }
        } else {
            // The empty-root branch (`hulltree.cpp:297-302`): the node is
            // re-initialised in place with the plane as its divider.
            self.nodes[idx] = Node::divider(*plane);
            marks.push(idx);
            return;
        }

        // We've looked at this plane so add it to the marked list
        // (`hulltree.cpp:305-309`). A node joins the list at most once per
        // hull — the reference's `pNextMarked_` stays non-NULL until the
        // final tag walk clears it — so this is a plain push here.
        marks.push(idx);

        // On top of us: just mark it (hulltree.cpp:312-336).
        if !(any_side[0] | any_side[1]) {
            let same_dir = self.nodes[idx].divider.normal().dot_product(plane.normal()) > 0.0;
            let sd = same_dir as i32;
            let nsd = 1 - sd;
            let node = &mut self.nodes[idx];
            // Mark it if we're allowed to mark this direction.
            if node.marked & (256 << sd) == 0 {
                node.marked |= 1 << sd;
            }
            // An edge plane of the hull being added: never consider the
            // other side again.
            node.marked &= !(1 << nsd);
            node.marked |= 256 << nsd;
            node.marked |= 65536;
            return;
        }

        // One side only, and we're not a boundary plane of the hull being
        // added: work out whether the side we're not on might still be
        // inside the hull (hulltree.cpp:396-440). The both-sides split code
        // above it is commented out in the reference; `inFront`/`outBack`
        // stay copies of `outline`, so both recursions take `outline`.
        if !(any_side[0] && any_side[1]) && self.nodes[idx].marked & 65536 == 0 {
            let side_we_are_not_on = any_side[0] as i32;
            let pointing_same_way =
                self.nodes[idx].divider.normal().dot_product(plane.normal()) > 0.0;
            if pointing_same_way ^ any_side[1] {
                if self.nodes[idx].marked & (256 << side_we_are_not_on) == 0 {
                    self.nodes[idx].marked |= 1 << side_we_are_not_on;
                }
            } else {
                self.nodes[idx].marked &= !(1 << side_we_are_not_on);
                self.nodes[idx].marked |= 256 << side_we_are_not_on;
            }
        }

        if any_side[0] {
            // Some out back: clear the back mark, bar anyone from setting it.
            self.nodes[idx].marked &= !1;
            self.nodes[idx].marked |= 256;
            let child = match self.nodes[idx].back {
                Some(c) => c,
                None => {
                    // `new HullTree(plane, firstMarked)`: marked 2|65536 and
                    // on the mark list from birth.
                    let c = self.nodes.len();
                    self.nodes.push(Node::divider(*plane));
                    self.nodes[idx].back = Some(c);
                    marks.push(c);
                    c
                }
            };
            self.add_plane(child, plane, outline, marks);
        }
        if any_side[1] {
            // Some in front: clear the front mark, bar anyone from setting it.
            self.nodes[idx].marked &= !2;
            self.nodes[idx].marked |= 512;
            let child = match self.nodes[idx].front {
                Some(c) => c,
                None => {
                    let c = self.nodes.len();
                    self.nodes.push(Node::divider(*plane));
                    self.nodes[idx].front = Some(c);
                    marks.push(c);
                    c
                }
            };
            self.add_plane(child, plane, outline, marks);
        }
    }

    /// `HullTree::testPoint` (`hulltree.cpp:475-499`): the tags whose hull
    /// contains the point. The reference prepends each node's tags onto the
    /// result chain; the port collects and reverses, which is the same order.
    pub fn test_point(&self, point: &Vector3) -> Vec<usize> {
        let mut tags = Vec::new();
        let mut idx = Some(0usize);
        while let Some(i) = idx {
            let node = &self.nodes[i];
            let out_back = !node.divider.is_in_front_of(point);
            let own = if out_back {
                &node.tag_back
            } else {
                &node.tag_front
            };
            tags.extend_from_slice(own);
            idx = if out_back { node.back } else { node.front };
        }
        tags.reverse();
        tags
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The six planes of an axis-aligned box, in the reference border shape:
    /// the `bounds_` planes face **inward** — the front side of each plane is
    /// the chunk interior (`chunk.cpp:695-806`, right = `(1,0,0), 0`).
    fn box_border(min: Vector3, max: Vector3) -> HullBorder {
        vec![
            PlaneEq::new(Vector3::new(1.0, 0.0, 0.0), min.x),
            PlaneEq::new(Vector3::new(-1.0, 0.0, 0.0), -max.x),
            PlaneEq::new(Vector3::new(0.0, 1.0, 0.0), min.y),
            PlaneEq::new(Vector3::new(0.0, -1.0, 0.0), -max.y),
            PlaneEq::new(Vector3::new(0.0, 0.0, 1.0), min.z),
            PlaneEq::new(Vector3::new(0.0, 0.0, -1.0), -max.z),
        ]
    }

    /// A single box hull: the interior tags, the exterior does not.
    #[test]
    fn a_box_hull_tags_its_interior_only() {
        let mut tree = HullTree::new();
        let border = box_border(Vector3::new(0.0, 0.0, 0.0), Vector3::new(10.0, 10.0, 10.0));
        tree.add(&border, 7);

        assert_eq!(tree.test_point(&Vector3::new(5.0, 5.0, 5.0)), vec![7]);
        // Inside but tight in a corner — the almostEqual buffer of
        // `PlaneEq::isInFrontOf` keeps an on-plane point inside.
        assert_eq!(tree.test_point(&Vector3::new(0.0, 0.0, 0.0)), vec![7]);
        // Outside every face.
        assert!(tree.test_point(&Vector3::new(15.0, 5.0, 5.0)).is_empty());
        assert!(tree.test_point(&Vector3::new(-1.0, 5.0, 5.0)).is_empty());
    }

    /// Two overlapping hulls: the overlap carries both tags, the exclusive
    /// parts carry their own, and none carry the other's.
    #[test]
    fn overlapping_hulls_stack_and_split_their_tags() {
        let mut tree = HullTree::new();
        tree.add(
            &box_border(Vector3::new(0.0, 0.0, 0.0), Vector3::new(10.0, 10.0, 10.0)),
            1,
        );
        tree.add(
            &box_border(Vector3::new(5.0, 0.0, 0.0), Vector3::new(15.0, 10.0, 10.0)),
            2,
        );

        let mut both = tree.test_point(&Vector3::new(7.0, 5.0, 5.0));
        both.sort_unstable();
        assert_eq!(both, vec![1, 2], "the overlap is in both hulls");
        assert_eq!(tree.test_point(&Vector3::new(2.0, 5.0, 5.0)), vec![1]);
        assert_eq!(tree.test_point(&Vector3::new(12.0, 5.0, 5.0)), vec![2]);
        assert!(tree.test_point(&Vector3::new(-5.0, 5.0, 5.0)).is_empty());
    }

    /// The same hull added twice marks the same nodes again: the point test
    /// reports the tag once per add (Column::add_chunk guards the duplicate
    /// before it reaches here, `base_chunk_space.cpp:427`).
    #[test]
    fn adding_the_same_hull_twice_marks_both_times() {
        let mut tree = HullTree::new();
        let border = box_border(Vector3::new(0.0, 0.0, 0.0), Vector3::new(10.0, 10.0, 10.0));
        tree.add(&border, 7);
        tree.add(&border, 7);
        assert_eq!(tree.test_point(&Vector3::new(5.0, 5.0, 5.0)), vec![7, 7]);
    }

    /// A degenerate border — two same-facing parallel planes cut the outline
    /// off entirely (`hulltree.cpp:151-155` kill branch) — adds without
    /// building any interior: the point test stays empty everywhere.
    #[test]
    fn a_parallel_cut_off_border_builds_no_interior() {
        let mut tree = HullTree::new();
        // z >= 0, x >= 0, x <= 5 — the two x planes pinch every outline.
        let border = vec![
            PlaneEq::new(Vector3::new(0.0, 0.0, 1.0), 0.0),
            PlaneEq::new(Vector3::new(1.0, 0.0, 0.0), 0.0),
            PlaneEq::new(Vector3::new(1.0, 0.0, 0.0), 5.0),
        ];
        tree.add(&border, 9);
        assert!(
            tree.test_point(&Vector3::new(2.0, 0.0, 2.0)).is_empty(),
            "no hull interior survives the parallel pinch"
        );
        assert!(tree.test_point(&Vector3::new(7.0, 0.0, 7.0)).is_empty());
        assert_eq!(tree.nodes.len(), 1, "no child nodes were created");
    }

    /// A deep split: the second hull's planes reach past the installed
    /// divider, so the descent both recurses into the existing child and
    /// grows a new one — the parallel `marked`/`in_chain` arrays are grown
    /// for the new node.
    #[test]
    fn a_split_grows_the_marks_arrays_for_the_new_nodes() {
        let mut tree = HullTree::new();
        tree.add(
            &box_border(Vector3::new(0.0, 0.0, 0.0), Vector3::new(10.0, 10.0, 10.0)),
            "root".to_string(),
        );
        tree.add(
            &box_border(Vector3::new(4.0, 0.0, 0.0), Vector3::new(14.0, 10.0, 10.0)),
            "split".to_string(),
        );
        let mut tags = tree.test_point(&Vector3::new(7.0, 5.0, 5.0));
        tags.sort();
        assert_eq!(tags, vec!["root".to_string(), "split".to_string()]);
    }
}
