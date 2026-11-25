//! BW-10-01-05 the hull tree (`lib/physics2/hulltree.{hpp,cpp}`): a BSP over
//! convex hull borders that answers "which hulls contain this point".
//!
//! `HullTree::add` walks the border planes, clips each plane's outline down
//! to the polygon its neighbours cut out (`Portal2D`-shaped scratch, but the
//! outline is a plain point list built by `addPoint`), lifts it back to 3D
//! and folds it into the tree by `addPlane`. `testPoint` then walks one
//! root-to-leaf path and chains the front/back tags of every node on it.
//!
//! Porting notes pinned by the reference:
//! * the mark chain (sentinel `(HullTree*)1`) collects the nodes a single
//!   `add` touched; tags are pushed onto `tagFront_`/`tagBack_` only after
//!   every plane has been folded in, and all mark bits clear at the end of
//!   the call;
//! * `marked_` bit 65536 marks nodes new in the current hull, 256/512 are
//!   "never mark this side" gates;
//! * the outline is **never clipped** when it straddles a divider: the
//!   splitting block (`hulltree.cpp:345-394`) is commented out in the
//!   reference, so both children receive the full outline. Porting the dead
//!   algorithm would change results, so the port keeps the pass-through;
//! * `testPoint` uses `PlaneEq::isInFrontOf`, where a point within the
//!   reference's 0.0004 epsilon of the plane counts as in front.

use foundation::math::{PlaneEq, Vector2, Vector3};

/// The mark bits (`hulltree.hpp:146-149`).
const MARK_BACK: u32 = 1;
const MARK_FRONT: u32 = 2;
const MARK_NOT_BACK: u32 = 256;
const MARK_NOT_FRONT: u32 = 512;
const MARK_NEW: u32 = 65536;

/// One hull entry: in the reference this is the `HullChunk` subclass of
/// `HullContents` holding the chunk pointer; the port holds the chunk's
/// identifier and resolves it in the owning column.
pub type HullTag = String;

/// A hull tree (`HullTree`, `hulltree.hpp:42-150`). The reference node
/// chain (`pFront_`/`pBack_` pointers plus the transient mark chain) lives
/// in an arena here: the tree owns its nodes and the per-`add` mark state
/// is a scratch parallel to it, with the same all-zero-between-calls
/// invariant the reference maintains by clearing every visited node.
#[derive(Debug, Default, Clone)]
pub struct HullTree {
    nodes: Vec<Node>,
}

#[derive(Debug, Clone)]
struct Node {
    /// `divider_` (`hulltree.hpp:135`): a zero normal means "no divider
    /// yet" — only the root starts there (`hulltree.cpp:64`).
    divider: PlaneEq,
    /// `pFront_` / `pBack_` (`hulltree.hpp:137-138`).
    front: Option<usize>,
    back: Option<usize>,
    /// `tagFront_` / `tagBack_` (`hulltree.hpp:140-141`).
    tag_front: Vec<HullTag>,
    tag_back: Vec<HullTag>,
}

impl Node {
    /// `HullTree()` (`hulltree.cpp:63-70`).
    fn new() -> Self {
        Self {
            divider: PlaneEq::new(Vector3::ZERO, 0.0),
            front: None,
            back: None,
            tag_front: Vec::new(),
            tag_back: Vec::new(),
        }
    }

    /// `HullTree(plane, firstMarked)` (`hulltree.cpp:76-84`): new planes
    /// are always marked on the front, and flagged new in this hull.
    fn marked_bits() -> u32 {
        MARK_FRONT | MARK_NEW
    }
}

/// The per-`add` mark chain state. `marked` mirrors the reference's
/// `marked_` field (cleared for every touched node when `add` ends),
/// `chain` mirrors the `firstMarked`/`pNextMarked_` linked list (push
/// appends; the walk goes head-first, i.e. in reverse push order), and
/// `in_chain` replaces the `pNextMarked_ != NULL` membership test.
#[derive(Debug)]
struct MarkState {
    marked: Vec<u32>,
    in_chain: Vec<bool>,
    chain: Vec<usize>,
}

impl MarkState {
    fn new(node_count: usize) -> Self {
        Self {
            marked: vec![0; node_count],
            in_chain: vec![false; node_count],
            chain: Vec::new(),
        }
    }

    /// `if (pNextMarked_ == NULL) { pNextMarked_ = firstMarked;
    /// firstMarked = this; }` (`hulltree.cpp:305-309`).
    fn link(&mut self, node: usize) {
        if !self.in_chain[node] {
            self.in_chain[node] = true;
            self.chain.push(node);
        }
    }

    /// Track nodes created while this `add` is running: the reference's
    /// `marked_` is a node member, so a fresh child always has one; the
    /// port's parallel arrays are grown to match.
    fn grow(&mut self, node_count: usize) {
        if node_count > self.marked.len() {
            self.marked.resize(node_count, 0);
            self.in_chain.resize(node_count, false);
        }
    }
}

impl HullTree {
    /// `HullTree()` (`hulltree.cpp:63-70`): an empty tree still has its
    /// root node, with no divider.
    pub fn new() -> Self {
        Self {
            nodes: vec![Node::new()],
        }
    }

    /// `testPoint` (`hulltree.cpp:475-499`): walk the single descent path
    /// to the leaf, chaining every visited node's tags for the side the
    /// point is on. The reference prepends each tag onto the result chain,
    /// so the head is the deepest node's first tag; the port returns the
    /// chain head-to-tail.
    pub fn test_point(&self, point: &Vector3) -> Vec<HullTag> {
        let mut chain: Vec<HullTag> = Vec::new();
        let mut current = Some(0usize);
        while let Some(index) = current {
            let node = &self.nodes[index];
            let out_back = !node.divider.is_in_front_of(point);
            let own = if out_back {
                &node.tag_back
            } else {
                &node.tag_front
            };
            chain.extend(own.iter().cloned());
            current = if out_back { node.back } else { node.front };
        }
        chain.reverse();
        chain
    }

    /// `add` (`hulltree.cpp:107-274`): fold every border plane into the
    /// tree and push the tag onto the marked sides afterwards.
    pub fn add(&mut self, border: &[PlaneEq], tag: HullTag) {
        let mut marks = MarkState::new(self.nodes.len());

        for (i, it) in border.iter().enumerate() {
            // project the other planes into lines in this plane's basis
            // (`hulltree.cpp:121-131`); only the parallel sentinel (an
            // exactly zero normal) is rejected.
            let mut lines = Vec::new();
            for (j, bt) in border.iter().enumerate() {
                if i == j {
                    continue;
                }
                let aline = it.intersect(bt);
                if aline.normal() != Vector2::ZERO {
                    lines.push(aline);
                }
            }

            // clip the line set down to the outline (`hulltree.cpp:133-184`)
            let mut outline = Vec::new();
            for i2 in 0..lines.len() {
                let mut min_dist = -1000000.0f32;
                let mut max_dist = 1000000.0f32;
                for j2 in 0..lines.len() {
                    if i2 == j2 {
                        continue;
                    }
                    // if it's parallel then deal with it specially
                    if lines[i2].is_parallel(&lines[j2]) {
                        // if it's a facing line then ignore it
                        if lines[i2].normal().dot_product(&lines[j2].normal()) < 0.0 {
                            continue;
                        }
                        // if it's outside us then ignore it too
                        if lines[i2].is_in_front_of(&lines[j2].param(0.0)) {
                            continue;
                        }
                        // okay we are completely cut off by this line
                        min_dist = 1.0;
                        max_dist = -1.0;
                        break;
                    }
                    let i_dist = lines[i2].intersect(&lines[j2]);
                    // see what side we intersect on
                    if lines[i2].is_min_cutter(&lines[j2]) {
                        if min_dist < i_dist {
                            min_dist = i_dist;
                        }
                    } else if max_dist > i_dist {
                        max_dist = i_dist;
                    }
                }

                // make sure there's an edge between these two planes
                if min_dist >= max_dist {
                    continue;
                }
                // add them to the outline, but only the max end (the min
                // end is commented out in the reference, `:180`)
                outline.push(lines[i2].param(max_dist));
            }

            // `Portal3D(plane, portal)` (`hulltree.cpp:657-663`): map the
            // 2-D outline onto the plane
            let outline3d: Vec<Vector3> = outline.iter().map(|p| it.param(p)).collect();
            self.add_plane(0, it, &outline3d, &mut marks);
        }

        // the mark walk (`hulltree.cpp:240-266`): head-first over the
        // chain, tagging both sides independently, clearing as we go. The
        // reference's nBack/nFront counters feed a commented-out print.
        for &node in marks.chain.iter().rev() {
            if marks.marked[node] & MARK_BACK != 0 {
                self.nodes[node].tag_back.push(tag.clone());
            }
            if marks.marked[node] & MARK_FRONT != 0 {
                self.nodes[node].tag_front.push(tag.clone());
            }
            marks.marked[node] = 0;
        }
    }

    /// `addPlane` (`hulltree.cpp:280-467`).
    fn add_plane(
        &mut self,
        node: usize,
        plane: &PlaneEq,
        outline: &[Vector3],
        marks: &mut MarkState,
    ) {
        let mut any_side = [false, false];

        if self.nodes[node].divider.normal() != &Vector3::ZERO {
            // classify the outline against the divider (`:285-302`); the
            // dead zone is ±0.01
            for point in outline {
                let d = self.nodes[node].divider.distance_to(point);
                if d > 0.01 {
                    any_side[1] = true;
                } else if d < -0.01 {
                    any_side[0] = true;
                }
            }
        } else {
            // `*this = HullTree( plane, firstMarked )` (`:299`): install
            // the plane as this node's divider (a zero-normal node never
            // has children) and link it in the chain
            self.nodes[node] = Node {
                divider: *plane,
                front: None,
                back: None,
                tag_front: Vec::new(),
                tag_back: Vec::new(),
            };
            marks.marked[node] = Node::marked_bits();
            marks.link(node);
            return;
        }

        // register this node in the mark chain (`:304-309`)
        marks.link(node);

        // if it's on top of us, we can just mark it (`:311-336`)
        if !(any_side[0] || any_side[1]) {
            let same_dir = self.nodes[node]
                .divider
                .normal()
                .dot_product(plane.normal())
                > 0.0;
            let m = marks.marked[node];
            // mark it if we're allowed to mark this direction
            if m & (MARK_NOT_BACK << same_dir as u32) == 0 {
                marks.marked[node] = m | (MARK_BACK << same_dir as u32);
            }
            // an edge plane of our own means we should never consider the
            // other side
            let other = (!same_dir) as u32;
            marks.marked[node] &= !(MARK_BACK << other);
            marks.marked[node] |= MARK_NOT_BACK << other;
            marks.marked[node] |= MARK_NEW;
            return;
        }

        if any_side[0] && any_side[1] {
            // The reference's split block (`:345-394`) is commented out, so
            // a straddling outline is passed to both children unclipped
            // (`inFront = outline; outBack = outline;`). Nothing to do:
            // `outline` is handed down whole below.
        } else if marks.marked[node] & MARK_NEW == 0 {
            // guess what's on the other side (`:396-440`)
            let side_we_are_not_on = any_side[0] as u32;
            let pointing_same_way = self.nodes[node]
                .divider
                .normal()
                .dot_product(plane.normal())
                > 0.0;
            if pointing_same_way != any_side[1] {
                // the unvisited side may still be inside the hull
                if marks.marked[node] & (MARK_NOT_BACK << side_we_are_not_on) == 0 {
                    marks.marked[node] |= MARK_BACK << side_we_are_not_on;
                }
            } else {
                // that side provably cannot be inside the hull
                marks.marked[node] &= !(MARK_BACK << side_we_are_not_on);
                marks.marked[node] |= MARK_NOT_BACK << side_we_are_not_on;
            }
        }

        // recurse (`:443-466`)
        if any_side[0] {
            marks.marked[node] &= !MARK_BACK;
            marks.marked[node] |= MARK_NOT_BACK;
            match self.nodes[node].back {
                Some(back) => self.add_plane(back, plane, outline, marks),
                None => {
                    let index = self.nodes.len();
                    self.nodes.push(Node {
                        divider: *plane,
                        front: None,
                        back: None,
                        tag_front: Vec::new(),
                        tag_back: Vec::new(),
                    });
                    self.nodes[node].back = Some(index);
                    marks.grow(self.nodes.len());
                    marks.marked[index] = Node::marked_bits();
                    marks.link(index);
                }
            }
        }
        if any_side[1] {
            marks.marked[node] &= !MARK_FRONT;
            marks.marked[node] |= MARK_NOT_FRONT;
            match self.nodes[node].front {
                Some(front) => self.add_plane(front, plane, outline, marks),
                None => {
                    let index = self.nodes.len();
                    self.nodes.push(Node {
                        divider: *plane,
                        front: None,
                        back: None,
                        tag_front: Vec::new(),
                        tag_back: Vec::new(),
                    });
                    self.nodes[node].front = Some(index);
                    marks.grow(self.nodes.len());
                    marks.marked[index] = Node::marked_bits();
                    marks.link(index);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A box border: six planes with inward normals — the chunk border
    /// convention (`chunk.cpp:675-807` builds e.g. the right boundary as
    /// `PlaneEq((1,0,0), 0)`), so the hull sits in the front half-space of
    /// every plane.
    fn box_border(min: Vector3, max: Vector3) -> Vec<PlaneEq> {
        vec![
            PlaneEq::new(Vector3::new(1.0, 0.0, 0.0), min.x),
            PlaneEq::new(Vector3::new(-1.0, 0.0, 0.0), -max.x),
            PlaneEq::new(Vector3::new(0.0, 1.0, 0.0), min.y),
            PlaneEq::new(Vector3::new(0.0, -1.0, 0.0), -max.y),
            PlaneEq::new(Vector3::new(0.0, 0.0, 1.0), min.z),
            PlaneEq::new(Vector3::new(0.0, 0.0, -1.0), -max.z),
        ]
    }

    #[test]
    fn an_empty_tree_answers_nothing() {
        let tree = HullTree::new();
        assert!(tree.test_point(&Vector3::new(0.5, 0.5, 0.5)).is_empty());
    }

    #[test]
    fn one_hull_answers_inside_and_not_outside() {
        let mut tree = HullTree::new();
        tree.add(
            &box_border(Vector3::ZERO, Vector3::new(10.0, 10.0, 10.0)),
            "box".to_string(),
        );
        assert_eq!(
            tree.test_point(&Vector3::new(5.0, 5.0, 5.0)),
            vec!["box".to_string()]
        );
        // outside every plane: the descent falls off the back of the root
        assert!(tree.test_point(&Vector3::new(15.0, 5.0, 5.0)).is_empty());
        // outside on the min side too (front descent, no tags there)
        assert!(tree.test_point(&Vector3::new(-5.0, 5.0, 5.0)).is_empty());
    }

    #[test]
    fn two_overlapping_hulls_both_answer_inside_the_overlap() {
        let mut tree = HullTree::new();
        tree.add(
            &box_border(Vector3::ZERO, Vector3::new(10.0, 10.0, 10.0)),
            "a".to_string(),
        );
        tree.add(
            &box_border(Vector3::new(5.0, 0.0, 0.0), Vector3::new(15.0, 10.0, 10.0)),
            "b".to_string(),
        );
        let mut inside_both = tree.test_point(&Vector3::new(7.5, 5.0, 5.0));
        inside_both.sort();
        assert_eq!(inside_both, vec!["a".to_string(), "b".to_string()]);
        assert_eq!(tree.test_point(&Vector3::new(2.0, 5.0, 5.0)), vec!["a"]);
        assert_eq!(tree.test_point(&Vector3::new(12.0, 5.0, 5.0)), vec!["b"]);
        assert!(tree.test_point(&Vector3::new(20.0, 5.0, 5.0)).is_empty());
    }

    #[test]
    fn the_same_hull_added_twice_keeps_both_tags() {
        let mut tree = HullTree::new();
        let border = box_border(Vector3::ZERO, Vector3::new(10.0, 10.0, 10.0));
        tree.add(&border, "first".to_string());
        // The second add's planes are coplanar with the installed dividers,
        // which drives the coplanar mark arm.
        tree.add(&border, "second".to_string());
        let mut tags = tree.test_point(&Vector3::new(5.0, 5.0, 5.0));
        tags.sort();
        assert_eq!(tags, vec!["first".to_string(), "second".to_string()]);
    }

    #[test]
    fn a_parallel_same_direction_plane_cuts_its_line_off() {
        // A degenerate border: the inward top plane of the box is parallel
        // and same-direction with an extra y = 6 plane (normal (0,-1,0),
        // d = -6). For the top plane's basis the extra line is parallel,
        // non-facing, and its param(0) is not in front of the top line —
        // so the top line is dropped from its own outline.
        let mut tree = HullTree::new();
        let mut border = box_border(Vector3::ZERO, Vector3::new(10.0, 4.0, 10.0));
        border.push(PlaneEq::new(Vector3::new(0.0, -1.0, 0.0), -6.0));
        tree.add(&border, "slab".to_string());
        // The inside point still resolves through the remaining planes.
        assert_eq!(
            tree.test_point(&Vector3::new(5.0, 1.0, 5.0)),
            vec!["slab".to_string()]
        );
    }

    #[test]
    fn disjoint_tags_never_leak_between_hulls() {
        let mut tree = HullTree::new();
        tree.add(
            &box_border(Vector3::new(0.0, 0.0, 0.0), Vector3::new(4.0, 4.0, 4.0)),
            "low".to_string(),
        );
        tree.add(
            &box_border(
                Vector3::new(10.0, 10.0, 10.0),
                Vector3::new(14.0, 14.0, 14.0),
            ),
            "high".to_string(),
        );
        assert_eq!(tree.test_point(&Vector3::new(2.0, 2.0, 2.0)), vec!["low"]);
        assert_eq!(
            tree.test_point(&Vector3::new(12.0, 12.0, 12.0)),
            vec!["high"]
        );
    }

    /// An opposite-facing coplanar plane folds into the coplanar mark arm
    /// with `same_dir` false, which clears the front bit and marks the
    /// back: the tag lands in `tagBack_` and answers on that side.
    #[test]
    fn an_opposite_facing_coplanar_plane_marks_the_back() {
        let mut tree = HullTree::new();
        tree.add(
            &[
                PlaneEq::new(Vector3::new(1.0, 0.0, 0.0), 0.0),
                PlaneEq::new(Vector3::new(-1.0, 0.0, 0.0), 5.0),
            ],
            "plane".to_string(),
        );
        // the second plane's fold marked the root's back side; the front
        // side carries no tag for this degenerate border
        assert!(tree.test_point(&Vector3::new(5.0, 0.0, 0.0)).is_empty());
        assert_eq!(
            tree.test_point(&Vector3::new(-5.0, 0.0, 0.0)),
            vec!["plane".to_string()]
        );
    }

    #[test]
    fn a_point_on_a_divider_counts_as_in_front() {
        let mut tree = HullTree::new();
        tree.add(
            &box_border(Vector3::ZERO, Vector3::new(10.0, 10.0, 10.0)),
            "box".to_string(),
        );
        // Exactly on the max-x face: within the epsilon, so in front of it.
        assert_eq!(
            tree.test_point(&Vector3::new(10.0, 5.0, 5.0)),
            vec!["box".to_string()]
        );
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
