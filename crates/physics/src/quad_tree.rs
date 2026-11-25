//! BW-10-07-04: the obstacle quadtree (`lib/physics2/quad_tree.hpp` /
//! `quad_tree.ipp`) and its line traversal (`QTTraversal`).
//!
//! The reference is a template over a `MEMBER_TYPE` whose elements are held
//! as `const MEMBER_TYPE *` pointers and whose range is computed by an
//! ADL-found `calculateQTRange` free function. The port stores `u32`
//! element handles instead of pointers — the caller (the chunk crate's
//! obstacle arena) owns the elements, so a handle here plays the reference
//! pointer's role exactly, dedup by shared identity included. The range
//! computation stays member-dependent, so `add` takes the precomputed
//! [`QtRange`] (the chunk crate's `calculate_qt_range` feeds it).
//!
//! Porting notes:
//! - `QuadTreeMemPool` is the reference's POD allocation trick; the port
//!   uses a plain `Vec` of nodes and indexes them with the same `u16`
//!   `NodeRef`/`INVALID_NODE` discipline.
//! - `del` is a stub that answers `false` in the reference itself
//!   (`quad_tree.ipp:762-767`, "TODO: Implement this!"); `removeFromRoot`,
//!   `testPoint` and the debug printers have no port-side caller and are
//!   registered unported.
//! - `nearCount`-style counters are plain `i32` here (the reference's
//!   `char` cannot overflow in reachable states).

use foundation::math::vector::Vector2;
use foundation::math::vector::Vector3;

/// `QUAD_BL` (`quad_tree.hpp:55`).
pub const QUAD_BL: usize = 0;
/// `QUAD_BR`.
pub const QUAD_BR: usize = 1;
/// `QUAD_TL`.
pub const QUAD_TL: usize = 2;
/// `QUAD_TR`.
pub const QUAD_TR: usize = 3;

/// `NodeRef` / `INVALID_NODE` (`quad_tree.hpp:318-320`).
pub const INVALID_NODE: u16 = 0xffff;

/// `QTTraversal::STACK_SIZE` (`quad_tree.hpp:498`).
const STACK_SIZE: usize = 20;

/// `isZero(float)` (`stdmf_minimal.hpp:113-136`): an exact zero test.
fn is_zero(f: f32) -> bool {
    f.abs() <= 0.0
}

/// `QTCoord` (`quad_tree.hpp:65-133`): a coordinate in the
/// `(1 << depth)`-wide quadtree grid.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct QtCoord {
    pub x: i32,
    pub y: i32,
}

impl QtCoord {
    /// `clipMin`: nothing outside the bottom and left sides.
    pub fn clip_min(&mut self) {
        self.x = self.x.max(0);
        self.y = self.y.max(0);
    }

    /// `clipMax(depth)`: nothing outside the right and top sides.
    pub fn clip_max(&mut self, depth: usize) {
        let max = (1 << depth) - 1;
        self.x = self.x.min(max);
        self.y = self.y.min(max);
    }

    /// `findChild(depth)`: the quadrant a child coord is in.
    pub fn find_child(&self, depth: usize) -> usize {
        let mask = 1i32 << depth;
        (((self.x & mask) + 2 * (self.y & mask)) >> depth) as usize
    }

    /// `offset(quad, depth)`: convert this coord to a child's coord.
    pub fn offset(&mut self, quad: usize, depth: usize) {
        self.x |= ((quad & 1) as i32) << depth;
        self.y |= ((quad > 1) as i32) << depth;
    }
}

/// `QTRange` (`quad_tree.hpp:139-204`): a range of quadtree coords,
/// `left <= right` and `bottom <= top`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct QtRange {
    pub left: i32,
    pub bottom: i32,
    pub right: i32,
    pub top: i32,
}

impl QtRange {
    /// `fills(depth)`: the range fills a whole node at the input depth.
    pub fn fills(&self, depth: usize) -> bool {
        let max = (1 << depth) - 1;
        self.left == 0 && self.bottom == 0 && self.right == max && self.top == max
    }

    /// `isValid(depth)`: only used for the `MF_ASSERT`s in `addElement`.
    pub fn is_valid(&self, depth: usize) -> bool {
        let max = 1i32 << depth;
        (0 <= self.left)
            && (self.left <= self.right)
            && (self.right < max)
            && (0 <= self.bottom)
            && (self.bottom <= self.top)
            && (self.top < max)
    }

    /// `inQuad(depth)`: the range intersects the quadtree at all.
    pub fn in_quad(&self, depth: usize) -> bool {
        let max = 1i32 << depth;
        0 <= self.right && 0 <= self.top && self.left < max && self.bottom < max
    }

    /// `clip(depth)`: pull the range inside the quadtree.
    pub fn clip(&mut self, depth: usize) {
        let mut lo = QtCoord {
            x: self.left,
            y: self.bottom,
        };
        let mut hi = QtCoord {
            x: self.right,
            y: self.top,
        };
        lo.clip_min();
        hi.clip_max(depth);
        self.left = lo.x;
        self.bottom = lo.y;
        self.right = hi.x;
        self.top = hi.y;
    }
}

/// `QuadTreeNode` (`quad_tree.hpp:314-363`): four child refs and the
/// elements landed in this node. Fresh children are `INVALID_NODE`
/// (`QuadTreeNode::init`, `quad_tree.hpp:325-334`) — a derived `Default`
/// would zero them into a self-loop on the root.
#[derive(Debug, Clone)]
pub struct QuadTreeNode {
    child: [u16; 4],
    elements: Vec<u32>,
}

impl Default for QuadTreeNode {
    fn default() -> Self {
        Self {
            child: [INVALID_NODE; 4],
            elements: Vec::new(),
        }
    }
}

impl QuadTreeNode {
    /// `getNumElements`.
    pub fn num_elements(&self) -> usize {
        self.elements.len()
    }

    /// `getElement`.
    pub fn element(&self, index: usize) -> u32 {
        self.elements[index]
    }
}

/// `QuadTree` (`quad_tree.hpp:383-455`): the obstacle tree of one column.
/// `x`/`z` is the bottom-left corner of the covered square and `range` its
/// side length; `depth` is the maximum subdivision depth.
#[derive(Debug, Clone)]
pub struct QuadTree {
    nodes: Vec<QuadTreeNode>,
    origin: Vector2,
    depth: usize,
    range: f32,
}

impl QuadTree {
    /// `QuadTree::QuadTree` (`quad_tree.ipp:99-133`): the root node is
    /// created up front, so `root()` is always node 0.
    pub fn new(x: f32, z: f32, depth: usize, range: f32) -> Self {
        let mut tree = Self {
            nodes: Vec::new(),
            origin: Vector2::new(x, z),
            depth,
            range,
        };
        tree.create_new_node();
        assert_eq!(tree.nodes.len(), 1);
        tree
    }

    pub fn depth(&self) -> usize {
        self.depth
    }

    pub fn range(&self) -> f32 {
        self.range
    }

    /// The bottom-left corner (`origin_`, private in the reference; the
    /// range computation lives caller-side here, so it needs the origin).
    pub fn origin(&self) -> Vector2 {
        self.origin
    }

    /// `root()`: created by the constructor, always 0.
    pub fn root(&self) -> u16 {
        0
    }

    /// `getChildRef`.
    pub fn child_ref(&self, node: u16, quad: usize) -> u16 {
        self.nodes[node as usize].child[quad]
    }

    /// `getNumElements`.
    pub fn num_elements(&self, node: u16) -> usize {
        self.nodes[node as usize].num_elements()
    }

    /// `getElement`.
    pub fn element(&self, node: u16, index: usize) -> u32 {
        self.nodes[node as usize].element(index)
    }

    /// `createNewNode` (`quad_tree.ipp:242-253`).
    fn create_new_node(&mut self) -> u16 {
        let node_ref = self.nodes.len() as u16;
        assert!((node_ref as usize) < INVALID_NODE as usize);
        self.nodes.push(QuadTreeNode::default());
        node_ref
    }

    /// `getOrCreateChild` (`quad_tree.ipp:256-270`).
    fn get_or_create_child(&mut self, node_ref: u16, quad: usize) -> u16 {
        let child = self.child_ref(node_ref, quad);
        if child == INVALID_NODE {
            let child = self.create_new_node();
            self.nodes[node_ref as usize].child[quad] = child;
            child
        } else {
            child
        }
    }

    /// `QuadTree::add` (`quad_tree.ipp:150-172`) with the range
    /// precomputed caller-side: out-of-quadtree ranges are skipped (the
    /// reference `dprintf`s a warning there), the rest is clipped into the
    /// tree and recursed.
    pub fn add(&mut self, element: u32, range: QtRange) {
        if !range.in_quad(self.depth) {
            // dprintf "QuadTree::add: Skipping - Out of range ..."
            return;
        }

        let mut range = range;
        range.clip(self.depth);

        let root = self.root();
        self.add_element(element, root, range, self.depth);
    }

    /// `QuadTree::addToRoot` (`quad_tree.ipp:184-189`).
    pub fn add_to_root(&mut self, element: u32) {
        let root = self.root();
        self.nodes[root as usize].elements.push(element);
    }

    /// `QuadTree::addElement` recursion
    /// (`quad_tree.ipp:719-803`): a range that fills the node lands here;
    /// otherwise the range is split across the children it overlaps, each
    /// child's copy clipped to that child.
    fn add_element(&mut self, element: u32, node: u16, range: QtRange, depth: usize) {
        assert!(range.is_valid(depth));

        if range.fills(depth) {
            self.nodes[node as usize].elements.push(element);
            return;
        }

        let new_depth = depth - 1;
        let mask = 1i32 << new_depth;
        let clear = !(1i32 << new_depth);
        let max = (1i32 << new_depth) - 1;

        let in_left = (range.left & mask) == 0;
        let in_bottom = (range.bottom & mask) == 0;
        let in_right = (range.right & mask) != 0;
        let in_top = (range.top & mask) != 0;

        let mut orig_range = range;
        orig_range.left &= clear;
        orig_range.bottom &= clear;
        orig_range.right &= clear;
        orig_range.top &= clear;
        let mut new_range = orig_range;

        if in_left {
            if in_right {
                new_range.right = max; // Right clipped
            }

            if in_top {
                if in_bottom {
                    new_range.bottom = 0; // Left and bottom clipped
                }
                assert!(new_range.is_valid(new_depth));
                let child = self.get_or_create_child(node, QUAD_TL);
                self.add_element(element, child, new_range, new_depth);
                new_range.bottom = orig_range.bottom;
                new_range.top = max;
                // Now right and top clipped
            }

            if in_bottom {
                assert!(new_range.is_valid(new_depth));
                let child = self.get_or_create_child(node, QUAD_BL);
                self.add_element(element, child, new_range, new_depth);
            }

            new_range.right = orig_range.right; // Now just top clipped
        }

        if in_right {
            if in_left {
                new_range.left = 0;
            }
            if in_top {
                new_range.top = max;
            }
            // Left and top clipped

            if in_bottom {
                assert!(new_range.is_valid(new_depth));
                let child = self.get_or_create_child(node, QUAD_BR);
                self.add_element(element, child, new_range, new_depth);
                new_range.bottom = 0;
                // Left, top and bottom clipped
            }

            if in_top {
                new_range.top = orig_range.top;
                // Left and bottom clipped.
                assert!(new_range.is_valid(new_depth));
                let child = self.get_or_create_child(node, QUAD_TR);
                self.add_element(element, child, new_range, new_depth);
            }
        }
    }

    /// `QuadTree::traverse` (`quad_tree.ipp:377-387`): a traversal of the
    /// elements the (possibly widened) line from `src` to `dst` passes.
    pub fn traverse(&self, src: &Vector3, dst: &Vector3, radius: f32) -> QtTraversal<'_> {
        QtTraversal::new(self, src, dst, radius)
    }
}

/// One entry of the traversal stack (`StackElement`).
#[derive(Debug, Clone, Copy)]
struct StackElement {
    node_ref: u16,
    coord: QtCoord,
    depth: usize,
}

/// `QTTraversal` (`quad_tree.hpp:462-526`): walks the quadtree along the
/// query line, widening it by `radius` on both sides, visiting each
/// element of every node the widened line touches. The interval is
/// parameterised as `src + t*dir` for `t` in `[0, 1]`; the width makes it a
/// low line and a high line, each with its own crossing parameters.
pub struct QtTraversal<'a> {
    tree: &'a QuadTree,
    stack: [StackElement; STACK_SIZE],
    size: usize,
    head_index: usize,

    x_low0: f32,
    y_low0: f32,
    x_high0: f32,
    y_high0: f32,
    x_low_delta: f32,
    y_low_delta: f32,
    x_high_delta: f32,
    y_high_delta: f32,
    dir_type: i32,
}

impl<'a> QtTraversal<'a> {
    /// `QTTraversal::QTTraversal` (`quad_tree.ipp:399-523`). The query
    /// direction is flipped so every calculation can assume the line
    /// points towards the top-right; `dirType_` remembers the flip.
    fn new(tree: &'a QuadTree, src: &Vector3, dst: &Vector3, radius: f32) -> Self {
        let depth = tree.depth();
        let range = tree.range();
        let origin = tree.origin();

        let root = QtCoord { x: 0, y: 0 };
        let mut stack = [StackElement {
            node_ref: 0,
            coord: root,
            depth: 0,
        }; STACK_SIZE];
        stack[0] = StackElement {
            node_ref: tree.root(),
            coord: root,
            depth,
        };

        // The interval from src to dst: src + t * dir, t in [0, 1].
        let mut dir = Vector2::new(dst.x - src.x, dst.z - src.z);

        // Which way the line points: dirType_ is 2*pointsDown + pointsLeft.
        let points_left = dir.x < 0.0;
        let points_down = dir.y < 0.0;
        let dir_type = 2 * points_down as i32 + points_left as i32;

        // A wide line has a high side and a low side.
        let mut dir_norm = dir;
        dir_norm.normalise();
        dir_norm = dir_norm * radius;
        let mut perp = Vector2::new(-dir_norm.y, dir_norm.x);

        // Flips so the calculations can assume top-right; for the
        // top-left / bottom-right directions the perpendicular is the
        // opposite of what the math below assumes.
        if points_down != points_left {
            perp = perp * -1.0;
        }

        dir = dir + dir_norm * 2.0;

        let src_low = Vector2::new(src.x, src.z) - dir_norm - perp;
        let src_high = Vector2::new(src.x, src.z) - dir_norm + perp;
        // (The reference also computes dstLow/dstHigh and never reads
        // them — dead locals there, not carried.)

        let width = 1usize << depth;

        // Needs to be big but when you divide by 0.5 (i.e. double) it must
        // not go to infinity.
        let big_float = f32::MAX / 4.0;

        let mut t = Self {
            tree,
            stack,
            size: 1,
            head_index: 0,
            x_low0: 0.0,
            y_low0: 0.0,
            x_high0: 0.0,
            y_high0: 0.0,
            x_low_delta: 0.0,
            y_low_delta: 0.0,
            x_high_delta: 0.0,
            y_high_delta: 0.0,
            dir_type,
        };

        if !is_zero(dir.x) {
            // Solving 0 = (srcLow.x - origin.x) + t * dir.x
            t.x_low0 = (origin.x + points_left as i32 as f32 * range - src_low.x) / dir.x;
            t.x_high0 = (origin.x + points_left as i32 as f32 * range - src_high.x) / dir.x;

            // How much t changes for each step in the grid.
            t.x_low_delta = range / width as f32 / dir.x.abs();
            t.x_high_delta = t.x_low_delta;
        } else {
            // Degenerate: parallel to the y-axis. t becomes a big negative
            // for coordinates below the line and a big positive above.
            // (The x-axis case differs slightly from the y-axis one: the
            // high crossing can sit below the low crossing here.)
            let low_crossing = (width as f32 * (src_low.x - origin.x) / range).floor() + 0.5;
            t.x_low0 = -big_float;
            t.x_low_delta = -t.x_low0 / low_crossing;

            let high_crossing =
                low_crossing.min((width as f32 * (src_high.x - origin.x) / range).floor() + 0.5);
            t.x_high0 = if high_crossing > 0.0 {
                -big_float
            } else {
                big_float
            };
            t.x_high_delta = -t.x_high0 / high_crossing;
        }

        // The y-coordinate, done like the x-coordinate.
        if !is_zero(dir.y) {
            t.y_low0 = (origin.y + points_down as i32 as f32 * range - src_low.y) / dir.y;
            t.y_high0 = (origin.y + points_down as i32 as f32 * range - src_high.y) / dir.y;
            t.y_low_delta = range / width as f32 / dir.y.abs();
            t.y_high_delta = t.y_low_delta;
        } else {
            let low_crossing = (width as f32 * (src_low.y - origin.y) / range).floor() + 0.5;
            t.y_low0 = if low_crossing > 0.0 {
                -big_float
            } else {
                big_float
            };
            t.y_low_delta = -t.y_low0 / low_crossing;

            let high_crossing =
                low_crossing.max((width as f32 * (src_high.y - origin.y) / range).floor() + 0.5);
            t.y_high0 = -big_float;
            t.y_high_delta = -t.y_high0 / high_crossing;
        }

        t
    }

    /// `pushChild` (`quad_tree.ipp:525-545`): the calculations all assume
    /// a bottom-left to top-right line, so the real quadrant is the
    /// virtual one XORed with `dirType_`.
    fn push_child(&mut self, curr: &StackElement, quad: usize) {
        let node_ref = self
            .tree
            .child_ref(curr.node_ref, (quad ^ self.dir_type as usize) & 3);

        if node_ref != INVALID_NODE {
            let mut new_coord = curr.coord;

            // Still pretending it's the flipped coord system.
            new_coord.offset(quad, curr.depth - 1);
            self.push(node_ref, new_coord, curr.depth - 1);
        }
    }

    /// `push` (`quad_tree.ipp:655-668`).
    fn push(&mut self, node: u16, coord: QtCoord, depth: usize) {
        assert!(self.size < STACK_SIZE);
        assert!(self.head_index == 0);

        self.stack[self.size] = StackElement {
            node_ref: node,
            coord,
            depth,
        };
        self.size += 1;
    }

    /// `pop` (`quad_tree.ipp:671-680`).
    fn pop(&mut self) -> StackElement {
        self.head_index = 0;
        self.size -= 1;
        self.stack[self.size]
    }

    /// `processHead` (`quad_tree.ipp:689-715`): the head node's next
    /// unvisited element, if any. (The reference's `shouldConsider` is
    /// constant true — a pinned TODO there.)
    fn process_head(&mut self) -> Option<u32> {
        let head = self.stack[self.size - 1];
        let num_elements = self.tree.num_elements(head.node_ref);
        if self.head_index < num_elements {
            let next = self.tree.element(head.node_ref, self.head_index);
            self.head_index += 1;
            return Some(next);
        }
        None
    }

    /// `QTTraversal::next` (`quad_tree.ipp:548-648`): the next element
    /// along the line, or `None` when the traversal is done. Children are
    /// pushed quad 3 first (done last) down to quad 0 last (done first),
    /// each gate checking whether the widened line reaches the child.
    // The reference name is kept (`next()`), not the `Iterator` trait
    // shape — the traversal is consumed by an explicit while-let, as the
    // chunk-space walk does.
    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> Option<u32> {
        if self.size == 0 {
            return None;
        }

        while self.size > 0 {
            // Check if the head has things to traverse.
            if let Some(result) = self.process_head() {
                return Some(result);
            }

            // Pop the stack.
            let curr = self.pop();

            if curr.depth > 0 {
                let coord = curr.coord;
                // The number of grid squares in half of this current node.
                let half_width = (1 << (curr.depth - 1)) as f32;

                // How much t changes across any child node; low and high
                // differ only in the axis-parallel degenerate case.
                let curr_low_x_delta = half_width * self.x_low_delta;
                let curr_low_y_delta = half_width * self.y_low_delta;
                let curr_high_x_delta = half_width * self.x_high_delta;
                let curr_high_y_delta = half_width * self.y_high_delta;

                // t-values for the node's own border lines
                // (x=min, x=mid, x=max, y=min, y=mid, y=max).
                let x_min_low = self.x_low0 + coord.x as f32 * self.x_low_delta;
                let x_mid_low = x_min_low + curr_low_x_delta;

                let y_min_low = self.y_low0 + coord.y as f32 * self.y_low_delta;
                let y_mid_low = y_min_low + curr_low_y_delta;
                let y_max_low = y_mid_low + curr_low_y_delta;

                let x_min_high = self.x_high0 + coord.x as f32 * self.x_high_delta;
                let x_mid_high = x_min_high + curr_high_x_delta;
                let x_max_high = x_mid_high + curr_high_x_delta;

                let y_min_high = self.y_high0 + coord.y as f32 * self.y_high_delta;
                let y_mid_high = y_min_high + curr_high_y_delta;

                // Does the line go above / below the mid-point?
                let above_midpoint = y_mid_high <= x_mid_high;
                let below_midpoint = x_mid_low <= y_mid_low;

                let above_q0 = y_mid_low < x_min_low;
                let below_q0 = x_mid_high < y_min_high;

                let above_q3 = y_max_low < x_mid_low;
                let below_q3 = x_max_high < y_mid_high;

                // Is the interval entirely in one half of the region?
                let to_left = x_mid_low > 1.0;
                let to_right = x_mid_high < 0.0;
                let is_above = y_mid_low < 0.0;
                let is_below = y_mid_high > 1.0;

                // Push the children the line intersects. Quad 3 is pushed
                // first, so done last; quad 0 pushed last, done first.
                if !above_q3 && !below_q3 && !to_left && !is_below {
                    self.push_child(&curr, 3);
                }

                if below_midpoint && !to_left && !is_above {
                    self.push_child(&curr, 1);
                }

                if above_midpoint && !to_right && !is_below {
                    self.push_child(&curr, 2);
                }

                if !above_q0 && !below_q0 && !to_right && !is_above {
                    self.push_child(&curr, 0);
                }
            }
        }

        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(x: f32, y: f32, z: f32) -> Vector3 {
        Vector3::new(x, y, z)
    }

    #[test]
    fn qt_coord_helpers_follow_the_reference_bit_math() {
        let mut c = QtCoord { x: -1, y: -1 };
        c.clip_min();
        assert_eq!(c, QtCoord { x: 0, y: 0 });

        let mut c = QtCoord { x: 100, y: 100 };
        c.clip_max(2); // MAX = 3
        assert_eq!(c, QtCoord { x: 3, y: 3 });

        // findChild: x bit -> +1, y bit -> +2 (BL/BR/TL/TR order)
        assert_eq!(QtCoord { x: 0, y: 0 }.find_child(1), QUAD_BL);
        assert_eq!(QtCoord { x: 2, y: 0 }.find_child(1), QUAD_BR);
        assert_eq!(QtCoord { x: 0, y: 2 }.find_child(1), QUAD_TL);
        assert_eq!(QtCoord { x: 2, y: 2 }.find_child(1), QUAD_TR);

        // offset descends into a child: depth's bit set per quad
        let mut c = QtCoord { x: 0, y: 0 };
        c.offset(QUAD_TL, 0);
        assert_eq!(c, QtCoord { x: 0, y: 1 });
        let mut c = QtCoord { x: 0, y: 0 };
        c.offset(QUAD_BR, 0);
        assert_eq!(c, QtCoord { x: 1, y: 0 });
    }

    #[test]
    fn qt_range_answers_fill_valid_and_in_quad_like_the_reference() {
        let full = QtRange {
            left: 0,
            bottom: 0,
            right: 3,
            top: 3,
        };
        assert!(full.fills(2));
        assert!(!full.fills(3));
        assert!(full.is_valid(2));
        assert!(full.in_quad(2));

        // inQuad only asks whether the range reaches the quadtree at all
        let reaching = QtRange {
            left: -5,
            bottom: -5,
            right: 1,
            top: 1,
        };
        assert!(reaching.in_quad(2));
        assert!(!reaching.is_valid(2));
        assert!(
            !QtRange {
                left: -5,
                bottom: -5,
                right: -1,
                top: -1
            }
            .in_quad(2)
        );

        // clip pulls the range inside
        let mut clipped = reaching;
        clipped.clip(2);
        assert_eq!(
            clipped,
            QtRange {
                left: 0,
                bottom: 0,
                right: 1,
                top: 1
            }
        );
    }

    #[test]
    fn a_tree_starts_with_only_the_root_node() {
        let tree = QuadTree::new(0.0, 0.0, 3, 100.0);
        assert_eq!(tree.root(), 0);
        assert_eq!(tree.depth(), 3);
        assert_eq!(tree.range(), 100.0);
        assert_eq!(tree.origin(), Vector2::new(0.0, 0.0));
        assert_eq!(tree.num_elements(0), 0);
    }

    #[test]
    fn an_in_range_element_lands_in_the_root_when_it_fills() {
        let mut tree = QuadTree::new(0.0, 0.0, 2, 4.0);
        // a range covering the whole tree fills the root node
        tree.add(
            7,
            QtRange {
                left: 0,
                bottom: 0,
                right: 3,
                top: 3,
            },
        );
        assert_eq!(tree.num_elements(0), 1);
        assert_eq!(tree.element(0, 0), 7);
    }

    #[test]
    fn an_out_of_range_element_is_skipped() {
        let mut tree = QuadTree::new(0.0, 0.0, 2, 4.0);
        // entirely below/left of the quadtree: inQuad false -> skipped
        tree.add(
            1,
            QtRange {
                left: -9,
                bottom: -9,
                right: -5,
                top: -5,
            },
        );
        assert_eq!(tree.num_elements(0), 0);
    }

    /// A small range in one corner descends to a leaf: the child chain is
    /// created on demand and the element lands once. (Reference dump:
    /// root BL child is an empty depth-1 node, its BL child is the leaf.)
    #[test]
    fn a_corner_element_descends_to_a_leaf_child() {
        let mut tree = QuadTree::new(0.0, 0.0, 2, 4.0);
        tree.add(
            5,
            QtRange {
                left: 0,
                bottom: 0,
                right: 0,
                top: 0,
            },
        );
        // the root stays empty; the BL chain holds the element
        assert_eq!(tree.num_elements(0), 0);
        let bl = tree.child_ref(0, QUAD_BL);
        assert_ne!(bl, INVALID_NODE);
        assert_eq!(tree.num_elements(bl), 0);
        // TL/TR/BR are never created
        assert_eq!(tree.child_ref(0, QUAD_TL), INVALID_NODE);
        assert_eq!(tree.child_ref(0, QUAD_TR), INVALID_NODE);
        assert_eq!(tree.child_ref(0, QUAD_BR), INVALID_NODE);
        // the BL child of the depth-1 node is the depth-0 leaf
        let leaf = tree.child_ref(bl, QUAD_BL);
        assert_ne!(leaf, INVALID_NODE);
        assert_eq!(tree.num_elements(leaf), 1);
        assert_eq!(tree.element(leaf, 0), 5);
    }

    /// Elements of a subtree, recursive (the reference `countElements`
    /// debugging face).
    fn subtree_elements(tree: &QuadTree, node: u16) -> usize {
        let mut total = tree.num_elements(node);
        for quad in 0..4 {
            let child = tree.child_ref(node, quad);
            if child != INVALID_NODE {
                total += subtree_elements(tree, child);
            }
        }
        total
    }

    /// A range straddling the centre lands in all four children — the
    /// multi-child arms of the addElement split. Each child holds exactly
    /// one depth-0 leaf copy (the opposite corner leaf of itself).
    /// (Depth 1 cannot show this: any clipped range covering the whole
    /// tree `fills` the root, so the split runs at depth 2.)
    #[test]
    fn a_centred_range_lands_in_all_four_children() {
        let mut tree = QuadTree::new(0.0, 0.0, 2, 4.0);
        // a 2x2 block over the centre of the 4x4 grid
        tree.add(
            1,
            QtRange {
                left: 1,
                bottom: 1,
                right: 2,
                top: 2,
            },
        );
        for quad in [QUAD_BL, QUAD_BR, QUAD_TL, QUAD_TR] {
            let child = tree.child_ref(0, quad);
            assert_ne!(child, INVALID_NODE, "quad {quad}");
            assert_eq!(tree.num_elements(child), 0, "quad {quad}");
            assert_eq!(subtree_elements(&tree, child), 1, "quad {quad}");
        }
        assert_eq!(tree.num_elements(0), 0);
    }

    /// A range straddling one axis lands in exactly two children.
    #[test]
    fn an_x_straddling_range_lands_in_two_children() {
        let mut tree = QuadTree::new(0.0, 0.0, 1, 4.0);
        // covers both bottom cells but not the top: no fill, BL + BR
        tree.add(
            1,
            QtRange {
                left: 0,
                bottom: 0,
                right: 1,
                top: 0,
            },
        );
        assert_eq!(tree.num_elements(tree.child_ref(0, QUAD_BL)), 1);
        assert_eq!(tree.num_elements(tree.child_ref(0, QUAD_BR)), 1);
        assert_eq!(tree.child_ref(0, QUAD_TL), INVALID_NODE);
        assert_eq!(tree.child_ref(0, QUAD_TR), INVALID_NODE);
    }

    #[test]
    fn add_to_root_lands_without_children() {
        let mut tree = QuadTree::new(0.0, 0.0, 2, 4.0);
        tree.add_to_root(3);
        tree.add_to_root(4);
        assert_eq!(tree.num_elements(0), 2);
        assert_eq!(tree.element(0, 1), 4);
        assert_eq!(tree.child_ref(0, QUAD_BL), INVALID_NODE);
    }

    /// A traversal over a tree with one corner element reports it once;
    /// the line hits it whether it runs over the corner or the traversal
    /// degenerates to a point.
    #[test]
    fn a_traversal_reports_elements_on_its_line() {
        let mut tree = QuadTree::new(0.0, 0.0, 2, 4.0);
        tree.add(
            11,
            QtRange {
                left: 3,
                bottom: 3,
                right: 3,
                top: 3,
            },
        );
        tree.add(
            12,
            QtRange {
                left: 0,
                bottom: 0,
                right: 0,
                top: 0,
            },
        );

        // along the main diagonal: both elements, corner-first (quad 0 of
        // the root is done first)
        let mut t = tree.traverse(&v(0.5, 0.0, 0.5), &v(3.5, 0.0, 3.5), 0.0);
        let mut seen = Vec::new();
        while let Some(e) = t.next() {
            seen.push(e);
        }
        assert_eq!(seen, vec![12, 11]);

        // the reverse direction sees the same set (elements, order follows
        // the flipped direction type)
        let mut t = tree.traverse(&v(3.5, 0.0, 3.5), &v(0.5, 0.0, 0.5), 0.0);
        let mut seen = Vec::new();
        while let Some(e) = t.next() {
            seen.push(e);
        }
        assert_eq!(seen, vec![11, 12]);

        // a line missing both cells sees nothing
        let mut t = tree.traverse(&v(0.5, 0.0, 3.5), &v(3.5, 0.0, 0.5), 0.0);
        assert!(t.next().is_none());
    }

    /// All four direction types walk their line (orders dumped from the
    /// reference template against the same tree). Element 0 fills the
    /// root; element 1 spans the whole right column so it descends into
    /// both BR and TR; 2 is the TL cell, 3 the TR cell. Ranges whose
    /// right/top touch the centre lines repeat in the traversal — the
    /// sweep callers dedup at the obstacle mark layer.
    #[test]
    fn all_four_direction_types_walk_their_line() {
        let mut tree = QuadTree::new(0.0, 0.0, 1, 4.0);
        tree.add(
            0,
            QtRange {
                left: 0,
                bottom: 0,
                right: 1,
                top: 1,
            },
        );
        tree.add(
            1,
            QtRange {
                left: 1,
                bottom: 0,
                right: 1,
                top: 1,
            },
        );
        tree.add(
            2,
            QtRange {
                left: 0,
                bottom: 1,
                right: 1,
                top: 1,
            },
        );
        tree.add(
            3,
            QtRange {
                left: 1,
                bottom: 1,
                right: 1,
                top: 1,
            },
        );

        let collect = |tree: &QuadTree, sx: f32, sz: f32, dx: f32, dz: f32| {
            let mut t = tree.traverse(&v(sx, 0.0, sz), &v(dx, 0.0, dz), 0.0);
            let mut seen = Vec::new();
            while let Some(e) = t.next() {
                seen.push(e);
            }
            seen
        };

        // BL -> TR (dirType 0)
        assert_eq!(collect(&tree, 0.5, 0.5, 3.5, 3.5), vec![0, 2, 1, 1, 2, 3]);
        // TR -> BL (dirType 3)
        assert_eq!(collect(&tree, 3.5, 3.5, 0.5, 0.5), vec![0, 1, 2, 3, 1, 2]);
        // TL -> BR (dirType 1)
        assert_eq!(collect(&tree, 0.5, 3.5, 3.5, 0.5), vec![0, 2, 1, 2, 3, 1]);
        // BR -> TL (dirType 2)
        assert_eq!(collect(&tree, 3.5, 0.5, 0.5, 3.5), vec![0, 1, 1, 2, 3, 2]);
    }

    /// An axis-parallel line exercises the degenerate isZero arms (the
    /// big-float crossing setup): it walks the cells it passes.
    #[test]
    fn an_axis_parallel_line_uses_the_degenerate_arms() {
        let mut tree = QuadTree::new(0.0, 0.0, 1, 4.0);
        tree.add(
            0,
            QtRange {
                left: 0,
                bottom: 0,
                right: 0,
                top: 0,
            },
        );
        tree.add(
            1,
            QtRange {
                left: 1,
                bottom: 0,
                right: 1,
                top: 0,
            },
        );

        // straight along +x at the bottom: both bottom cells, BL first
        let mut t = tree.traverse(&v(0.5, 0.0, 0.5), &v(3.5, 0.0, 0.5), 0.0);
        let mut seen = Vec::new();
        while let Some(e) = t.next() {
            seen.push(e);
        }
        assert_eq!(seen, vec![0, 1]);

        // straight along +z on the left: only the BL cell
        let mut t = tree.traverse(&v(0.5, 0.0, 0.5), &v(0.5, 0.0, 3.5), 0.0);
        let mut seen = Vec::new();
        while let Some(e) = t.next() {
            seen.push(e);
        }
        assert_eq!(seen, vec![0]);
    }

    /// A radius wider than a cell sweeps neighbours too.
    #[test]
    fn a_wide_line_reaches_neighbouring_cells() {
        let mut tree = QuadTree::new(0.0, 0.0, 1, 4.0);
        tree.add(
            0,
            QtRange {
                left: 0,
                bottom: 0,
                right: 0,
                top: 0,
            },
        );
        tree.add(
            2,
            QtRange {
                left: 0,
                bottom: 1,
                right: 0,
                top: 1,
            },
        );

        // a point sweep along x at z=0.5 misses the top-left cell...
        let mut t = tree.traverse(&v(0.0, 0.0, 0.5), &v(4.0, 0.0, 0.5), 0.0);
        let mut seen = Vec::new();
        while let Some(e) = t.next() {
            seen.push(e);
        }
        assert_eq!(seen, vec![0]);

        // ...but a radius-2 sweep covers the whole column
        let mut t = tree.traverse(&v(0.0, 0.0, 0.5), &v(4.0, 0.0, 0.5), 2.0);
        let mut seen = Vec::new();
        while let Some(e) = t.next() {
            seen.push(e);
        }
        assert_eq!(seen, vec![0, 2]);
    }

    /// The obstacle-tree face used by the space walk: elements added
    /// through ranges derived from world-space boxes come back from a
    /// world-space traversal, dedup-free and in walk order.
    #[test]
    fn a_world_space_round_trip_maps_boxes_to_cells() {
        // the fixture column: origin (0,0), range 100 (gridSize), depth 5
        let mut tree = QuadTree::new(0.0, 0.0, 5, 100.0);
        let width = (1 << 5) as f32;
        let qt = |min_x: f32, min_z: f32, max_x: f32, max_z: f32| QtRange {
            left: (width * (min_x - 0.0) / 100.0) as i32,
            bottom: (width * (min_z - 0.0) / 100.0) as i32,
            right: (width * (max_x - 0.0) / 100.0) as i32,
            top: (width * (max_z - 0.0) / 100.0) as i32,
        };
        // a 10x10 box at the origin and one centred in the cell
        tree.add(0, qt(0.0, 0.0, 10.0, 10.0));
        tree.add(1, qt(45.0, 45.0, 55.0, 55.0));

        // a sweep through both boxes reports both; the centred box spans
        // four cells (range (14,14,17,17)) so the traversal repeats it
        // once per leaf — the space walk dedups through the obstacle
        // mark, not here (reference dump order)
        let mut t = tree.traverse(&v(0.0, 0.0, 0.0), &v(100.0, 0.0, 100.0), 0.0);
        let mut seen = Vec::new();
        while let Some(e) = t.next() {
            seen.push(e);
        }
        assert_eq!(seen, vec![0, 1, 1, 1, 1]);
    }
}
