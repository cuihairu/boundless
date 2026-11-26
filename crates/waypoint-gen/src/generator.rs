//! The BSP waypoint polygon generator (`WaypointGenerator`,
//! `waypoint_generator.cpp`). The flood grids land in
//! [`WaypointGenerator::init`], then [`WaypointGenerator::generate`]
//! subdivides the grid rectangle into a BSP tree of passable nodes
//! (`processNode`), turns the node edges into a shared point set
//! (`generatePoints`), stitches polygons from those points
//! (`generatePolygons`), links them across shared edges
//! (`generateAdjacencies`) and merges neighbours
//! (`joinPolygons`). [`WaypointGenerator::streamline`] collapses
//! doubled vertices left by the two-sided edge collection, and
//! [`WaypointGenerator::calculate_set_membership`] floods connectivity
//! sets from frontier points and drops isolated polygons.
//!
//! Dev-build `MF_ASSERT` sites compile out in the reference's release
//! builds (`-D_RELEASE`, the build the truth harness uses); the port
//! keeps the release semantics and asserts where the reference asserts.

use std::collections::BTreeSet;

use foundation::math::vector::Vector3;

use crate::flood::{AdjGridElt, MAX_HEIGHTS};
use crate::view::{NavVertex, NavView};

/// `GENERATOR_FLOAT_FUDGE` (`waypoint_generator.cpp:24`).
pub const GENERATOR_FLOAT_FUDGE: f32 = 0.001;

/// `EVEN_SPLIT_THRESHOLD` (`waypoint_generator.cpp:27`).
pub const EVEN_SPLIT_THRESHOLD: f32 = 25.0;

/// `MAX_HEIGHT_RANGE` (`waypoint_generator.cpp:30`).
pub const MAX_HEIGHT_RANGE: f32 = 3.0;

/// `HALF_ROOT_2`, local to the point-in-node checks; the reference's
/// own 7-digit constant, kept verbatim (`waypoint_generator.cpp:28`).
#[allow(clippy::approx_constant)]
const HALF_ROOT_2: f32 = 0.707107;

/// `SplitDef::VERTICAL_SPLIT` (`waypoint_generator.hpp:112`): the
/// normal that means "split on height".
const VERTICAL_SPLIT: i32 = 8;

/// `g_dx` (`waypoint_generator.cpp:16`).
const DX: [i32; 8] = [0, 1, 1, 1, 0, -1, -1, -1];

/// `g_dz` (`waypoint_generator.cpp:17`).
const DZ: [i32; 8] = [1, 1, 0, -1, -1, -1, 0, 1];

/// `g_normal`: the direction table normalised, computed by the
/// reference constructor (`waypoint_generator.cpp:200-207`).
fn normal(a: usize) -> (f32, f32) {
    let (dx, dz) = (DX[a] as f32, DZ[a] as f32);
    let inv = 1.0 / (dx * dx + dz * dz).sqrt();
    (dx * inv, dz * inv)
}

/// `cstdmf::almostEqual` default epsilon 0.0004 (`stdmf.hpp`).
fn almost_equal(a: f32, b: f32) -> bool {
    (a - b).abs() < 0.0004
}

/// `PointDef` (`waypoint_generator.hpp:122-131`): a point on a node
/// edge, parameterised by its line (`angle`, `offset`) and position
/// along it (`t`).
#[derive(Clone, Copy, Debug, Default)]
struct PointDef {
    angle: i32,
    offset: f32,
    t: f32,
    min_height: f32,
    max_height: f32,
}

impl PartialEq for PointDef {
    fn eq(&self, other: &Self) -> bool {
        self.cmp_ref(other) == std::cmp::Ordering::Equal
    }
}

impl Eq for PointDef {}

impl PointDef {
    /// Ordering (`PointDef::operator<`, `waypoint_generator.cpp:39-65`):
    /// angle strictly; `offset` and `t` only outside the float fudge;
    /// heights by exact `isEqual<float>` equality.
    fn cmp_ref(&self, other: &Self) -> std::cmp::Ordering {
        use std::cmp::Ordering;
        if self.angle < other.angle {
            return Ordering::Less;
        }
        if self.angle > other.angle {
            return Ordering::Greater;
        }
        if (self.offset - other.offset).abs() > GENERATOR_FLOAT_FUDGE {
            if self.offset < other.offset {
                return Ordering::Less;
            }
            if self.offset > other.offset {
                return Ordering::Greater;
            }
        }
        if (self.t - other.t).abs() > GENERATOR_FLOAT_FUDGE {
            return self.t.total_cmp(&other.t);
        }
        if self.min_height != other.min_height {
            return self.min_height.total_cmp(&other.min_height);
        }
        if self.max_height != other.max_height {
            return self.max_height.total_cmp(&other.max_height);
        }
        Ordering::Equal
    }

    /// `operator==` (`waypoint_generator.cpp:73-86`): the coarse fudge
    /// equality used to detect the walk's terminating point.
    fn eq_ref(&self, other: &Self) -> bool {
        self.angle == other.angle
            && (self.offset - other.offset).abs() < GENERATOR_FLOAT_FUDGE
            && (self.t - other.t).abs() < GENERATOR_FLOAT_FUDGE
    }

    /// `nearlySame` (`waypoint_generator.cpp:88-99`): the coarse 1.5
    /// fudge used by `nodesSame`.
    fn nearly_same(&self, other: &Self) -> bool {
        self.angle == other.angle
            && (self.offset - other.offset).abs() < 1.5
            && (self.t - other.t).abs() < 1.5
    }
}

impl PartialOrd for PointDef {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for PointDef {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.cmp_ref(other)
    }
}

/// `EdgeDef` (`waypoint_generator.hpp:189-196`).
#[derive(Clone, Copy, Debug)]
struct EdgeDef {
    from: (f32, f32),
    to: (f32, f32),
    id: i32,
}

impl PartialEq for EdgeDef {
    fn eq(&self, other: &Self) -> bool {
        self.cmp_ref(other) == std::cmp::Ordering::Equal
    }
}

impl Eq for EdgeDef {}

impl EdgeDef {
    /// `operator<` (`waypoint_generator.cpp:101-115`): exact position
    /// comparison, then id.
    fn cmp_ref(&self, other: &Self) -> std::cmp::Ordering {
        if self.from.0 != other.from.0 {
            return self.from.0.total_cmp(&other.from.0);
        }
        if self.from.1 != other.from.1 {
            return self.from.1.total_cmp(&other.from.1);
        }
        if self.to.0 != other.to.0 {
            return self.to.0.total_cmp(&other.to.0);
        }
        if self.to.1 != other.to.1 {
            return self.to.1.total_cmp(&other.to.1);
        }
        self.id.cmp(&other.id)
    }

    /// `operator==` (`waypoint_generator.cpp:118-132`): positions only,
    /// so the adjacency lookup finds every edge with the same span.
    fn eq_ref(&self, other: &Self) -> bool {
        self.from == other.from && self.to == other.to
    }
}

impl PartialOrd for EdgeDef {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for EdgeDef {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.cmp_ref(other)
    }
}

/// `BSPNode` (`waypoint_generator.hpp:143-168`).
#[derive(Clone, Debug, Default)]
pub(crate) struct BspNode {
    border_offset: [f32; 8],
    // kept verbatim from the reference node; the port never reads them
    // (the split line is recomputed from the border offsets)
    #[allow(dead_code)]
    split_offset: f32,
    #[allow(dead_code)]
    split_normal: i32,
    #[allow(dead_code)]
    parent: i32,
    front: i32,
    back: i32,
    pub(crate) waypoint: bool,
    waypoint_index: i32,
    centre: (f32, f32),
    pub(crate) min_height: f32,
    pub(crate) max_height: f32,
    base_x: i32,
    base_z: i32,
    width: i32,
    point_in_node_flags: Vec<bool>,
}

impl BspNode {
    /// `pointInNode(const Vector2&)`
    /// (`waypoint_generator.cpp:1346-1405`): border inclusive, 0.1
    /// slack, diagonal edges scaled by `HALF_ROOT_2`.
    fn point_in_node_f(&self, x: f32, z: f32) -> bool {
        if z + self.border_offset[0] < -0.1 {
            return false;
        }
        let hx = HALF_ROOT_2 * x;
        let hz = HALF_ROOT_2 * z;
        if hx + hz + self.border_offset[1] < -0.1 {
            return false;
        }
        if x + self.border_offset[2] < -0.1 {
            return false;
        }
        if hx - hz + self.border_offset[3] < -0.1 {
            return false;
        }
        if -z + self.border_offset[4] < -0.1 {
            return false;
        }
        if -hx - hz + self.border_offset[5] < -0.1 {
            return false;
        }
        if -x + self.border_offset[6] < -0.1 {
            return false;
        }
        if -hx + hz + self.border_offset[7] < -0.1 {
            return false;
        }
        true
    }

    /// `updatePointInNodeFlags`
    /// (`waypoint_generator.cpp:1284-1308`): cache the point-in-node
    /// answers for the integer scan loops.
    fn update_point_in_node_flags(&mut self) {
        let z1 = (-self.border_offset[0]) as i32;
        let z2 = self.border_offset[4] as i32;
        let x1 = (-self.border_offset[2]) as i32;
        let x2 = self.border_offset[6] as i32;

        self.base_x = x1 - 2;
        self.base_z = z1 - 2;
        self.width = (z2 + 2) - (z1 - 2);

        self.point_in_node_flags =
            vec![false; self.width as usize * ((x2 + 2) - (x1 - 2)) as usize];

        for x in x1..=x2 {
            for z in (self.base_z + 2)..=z2 {
                if self.point_in_node_f(x as f32, z as f32) {
                    let idx = ((x - self.base_x) * self.width + z - self.base_z) as usize;
                    self.point_in_node_flags[idx] = true;
                }
            }
        }
    }

    /// `pointInNode(int, int)` (`waypoint_generator.cpp:1317-1322`).
    fn point_in_node_i(&self, x: i32, z: i32) -> bool {
        self.point_in_node_flags[((x - self.base_x) * self.width + z - self.base_z) as usize]
    }

    /// `calcCentre` (`waypoint_generator.cpp:1329-1336`): half-extents
    /// rather than a position — consumers re-add the border offsets.
    fn calc_centre(&mut self) {
        self.centre = (
            (self.border_offset[6] + self.border_offset[2]) / 2.0,
            (self.border_offset[4] + self.border_offset[0]) / 2.0,
        );
    }
}

/// `VertexDef` (`waypoint_generator.hpp:133-146`).
#[derive(Clone, Copy, Debug)]
pub(crate) struct VertexDef {
    pub(crate) pos: (f32, f32),
    pub(crate) adj_nav_poly: i32,
    pub(crate) adj_to_another_chunk: bool,
    pub(crate) angles: i32,
}

/// `PolygonDef` (`waypoint_generator.hpp:148-164`): the default
/// constructor zeroes the heights and the set.
#[derive(Clone, Debug)]
pub(crate) struct PolygonDef {
    pub(crate) vertices: Vec<VertexDef>,
    pub(crate) min_height: f32,
    pub(crate) max_height: f32,
    pub(crate) set: i32,
}

impl Default for PolygonDef {
    fn default() -> Self {
        Self {
            vertices: Vec::new(),
            min_height: 0.0,
            max_height: 0.0,
            set: 0,
        }
    }
}

impl PolygonDef {
    /// `ptNearEnough` (`waypoint_generator.cpp:135-180`): every edge
    /// must have one of a 3x3 offset ring of the point on its inner
    /// side.
    fn pt_near_enough(&self, pt: (f32, f32, f32)) -> bool {
        if pt.1 > self.max_height + 1.5 || pt.1 < self.min_height - 0.5 {
            return false;
        }
        let mut ai = self.vertices.len() - 1;
        for bi in 0..self.vertices.len() {
            let a = self.vertices[ai].pos;
            let b = self.vertices[bi].pos;

            let u = b.0 - a.0;
            let v = b.1 - a.1;

            let mut xd = pt.0 - a.0;
            let mut zd = pt.2 - a.1;

            let mut any_inside = false;
            // before the first loop iteration this lands the probe at
            // (-1, -1) relative to the edge origin
            xd += 1.0;
            zd -= 2.0;
            for j in 0..9 {
                if j % 3 != 0 {
                    xd += 1.0;
                } else {
                    xd -= 2.0;
                    zd += 1.0;
                }
                if xd * v - zd * u >= 0.0 {
                    any_inside = true;
                }
            }
            if !any_inside {
                return false;
            }
            ai = bi;
        }
        true
    }
}

/// `SplitDef` (`waypoint_generator.hpp:110-120`): `position` and
/// `heights` are a C++ union — one storage, read as a position for
/// planar splits and as a height pair for vertical ones.
#[derive(Clone, Copy)]
struct SplitDef {
    normal: i32,
    value: f32,
    ph: [f32; 2],
}

impl Default for SplitDef {
    /// The reference default-constructs into garbage and immediately
    /// assigns every field it later reads; `value = -1` is the only
    /// observed initial value (`processNode`).
    fn default() -> Self {
        Self {
            normal: 0,
            value: -1.0,
            ph: [0.0; 2],
        }
    }
}

/// `WaypointGenerator` (`waypoint_generator.hpp:25`).
pub struct WaypointGenerator {
    grid_x: u32,
    grid_z: u32,
    grid_min: Vector3,
    grid_resolution: f32,
    hgt_grids: Vec<Vec<f32>>,
    adj_grids: Vec<Vec<AdjGridElt>>,
    pub(crate) bsp: Vec<BspNode>,
    pub(crate) polygons: Vec<PolygonDef>,
    points: BTreeSet<PointDef>,
    edges: BTreeSet<EdgeDef>,
    sets: i32,
}

impl Default for WaypointGenerator {
    fn default() -> Self {
        Self::new()
    }
}

impl WaypointGenerator {
    /// The reference constructor (`waypoint_generator.cpp:182-215`).
    pub fn new() -> Self {
        Self {
            grid_x: 0,
            grid_z: 0,
            grid_min: Vector3::default(),
            grid_resolution: 0.0,
            hgt_grids: (0..MAX_HEIGHTS).map(|_| Vec::new()).collect(),
            adj_grids: (0..MAX_HEIGHTS).map(|_| Vec::new()).collect(),
            bsp: Vec::new(),
            polygons: Vec::new(),
            points: BTreeSet::new(),
            edges: BTreeSet::new(),
            sets: 0,
        }
    }

    /// `clear` (`waypoint_generator.cpp:227-243`).
    fn clear(&mut self) {
        for g in 0..MAX_HEIGHTS {
            self.hgt_grids[g] = Vec::new();
            self.adj_grids[g] = Vec::new();
        }
        self.bsp.clear();
        self.points.clear();
        self.edges.clear();
        self.polygons.clear();
    }

    /// `init` (`waypoint_generator.cpp:339-357`).
    pub fn init(&mut self, width: i32, height: i32, grid_min: &Vector3, grid_resolution: f32) {
        self.clear();

        self.grid_x = width as u32;
        self.grid_z = height as u32;
        self.grid_min = *grid_min;
        self.grid_resolution = grid_resolution;

        let cells = (self.grid_x * self.grid_z) as usize;
        for g in 0..MAX_HEIGHTS {
            self.hgt_grids[g] = vec![0.0; cells];
            self.adj_grids[g] = vec![AdjGridElt::default(); cells];
        }
    }

    /// `hgtGrids()` (`waypoint_generator.hpp:52`).
    pub fn hgt_grids_mut(&mut self) -> &mut [Vec<f32>] {
        &mut self.hgt_grids
    }

    /// `adjGrids()` (`waypoint_generator.hpp:51`).
    pub fn adj_grids_mut(&mut self) -> &mut [Vec<AdjGridElt>] {
        &mut self.adj_grids
    }

    /// `getSets` (`waypoint_generator.cpp:1952`).
    pub fn sets(&self) -> i32 {
        self.sets
    }

    /// `generate` (`waypoint_generator.cpp:360-381`).
    pub fn generate(&mut self) {
        self.init_bsp();

        let mut index_stack = vec![0usize];
        while !index_stack.is_empty() {
            self.process_node(&mut index_stack);
        }

        self.generate_points();
        self.generate_polygons();
        self.generate_adjacencies();
        self.join_polygons();
    }

    /// `initBSP` (`waypoint_generator.cpp:384-422`).
    fn init_bsp(&mut self) {
        let mut node = BspNode {
            split_offset: 0.0,
            split_normal: 0,
            waypoint: false,
            base_x: 0,
            base_z: 0,
            width: 0,
            parent: -1,
            front: -1,
            back: -1,
            waypoint_index: -1,
            min_height: -13000.0,
            max_height: 13000.0,
            ..BspNode::default()
        };

        let min = (0.0f32, 0.0f32);
        let max = ((self.grid_x - 1) as f32, (self.grid_z - 1) as f32);

        node.border_offset[0] = -dot2(normal(0), min);
        node.border_offset[1] = -dot2(normal(1), min);
        node.border_offset[2] = -dot2(normal(2), min);
        node.border_offset[3] = -dot2(normal(3), (min.0, max.1));
        node.border_offset[4] = -dot2(normal(4), max);
        node.border_offset[5] = -dot2(normal(5), max);
        node.border_offset[6] = -dot2(normal(6), max);
        node.border_offset[7] = -dot2(normal(7), (max.0, min.1));

        node.calc_centre();
        self.reduce_node_height(&mut node);

        self.bsp.push(node);
    }

    /// `nodesSame` (`waypoint_generator.cpp:425-468`).
    fn nodes_same(&self, index1: usize, index2: usize) -> bool {
        for i in 0..8usize {
            let (res1, p1a, p1b) = self.calc_edge_pair(index1, i);
            let (res2, p2a, p2b) = self.calc_edge_pair(index2, i);

            // if the edge exists for one but not the other, not same
            if res1 != res2 {
                return false;
            }

            if res1 && (!(p1a.nearly_same(&p2a)) || !(p1b.nearly_same(&p2b))) {
                return false;
            }
        }
        true
    }

    /// `processNode` (`waypoint_generator.cpp:478-694`): the index
    /// stack's top is popped here, matching the reference's
    /// explicit-stack driver loop.
    fn process_node(&mut self, index_stack: &mut Vec<usize>) {
        let tgx = self.grid_x as i32;
        let dx = [0, 1, 1, 1, 0, -1, -1, -1];
        let dy = [tgx, tgx, 0, -tgx, -tgx, -tgx, 0, tgx];

        let mut passable = true;
        let mut best_split = SplitDef::default();
        let mut passable_count = 0i32;
        let mut impassable_count = 0i32;
        let mut nh = 0f32;
        let mut ah = 0f32;

        let index = index_stack.pop().expect("stack non-empty");

        best_split.value = -1.0;

        let z1 = (-self.bsp[index].border_offset[0]) as i32;
        let z2 = self.bsp[index].border_offset[4] as i32;
        let x1 = (-self.bsp[index].border_offset[2]) as i32;
        let x2 = self.bsp[index].border_offset[6] as i32;
        let y1 = self.bsp[index].min_height;
        let y2 = self.bsp[index].max_height;

        // Examine every point in the node. For each point, examine each
        // neighbour that is inside the node. For the node to be
        // passable, there must be an adjacency to every neighbour.
        for x in x1..=x2 {
            for z in z1..=z2 {
                if !self.bsp[index].point_in_node_f(x as f32, z as f32) {
                    continue;
                }

                let ptindex = x + self.grid_x as i32 * z;

                // make sure there is only one point at these coordinates
                // in the node
                let mut ng = -1i32;
                let mut g = 0usize;
                while g < MAX_HEIGHTS {
                    ah = self.hgt_grids[g][ptindex as usize];
                    if ah < y1 || ah > y2 {
                        g += 1;
                        continue;
                    }
                    if self.adj_grids[g][ptindex as usize].all == 0 {
                        g += 1;
                        continue;
                    }
                    // ok, this is our point. see if we already have one
                    if ng != -1 {
                        break;
                    }
                    // this is the first so record it
                    ng = g as i32;
                    nh = ah;
                    g += 1;
                }

                // see if we have overlapping points
                if g != MAX_HEIGHTS {
                    // suggest a split
                    let mut split = SplitDef {
                        normal: VERTICAL_SPLIT,
                        value: -1.0,
                        ph: [nh.min(ah), nh.max(ah)],
                    };
                    self.calc_split_value(&self.bsp[index], &mut split);
                    if split.value > best_split.value {
                        best_split = split;
                    }

                    // and let none pass
                    passable = false;
                    impassable_count += 8;
                    continue;
                }

                // set the mask then
                let mut mask = 0u32;
                if ng != -1 {
                    for a in 0..8usize {
                        let og = self.adj_grids[ng as usize][ptindex as usize].angle(a);
                        if og != 0 {
                            let h = self.hgt_grids[og as usize][(ptindex + dx[a] + dy[a]) as usize];
                            if h >= y1 && h <= y2 {
                                mask |= 1 << a;
                            }
                        }
                    }
                }

                // OK, now examine the adjacency mask
                if mask == 0x00 {
                    // This point has no adjacencies.
                    // No need to check them individually.
                    passable = false;
                    impassable_count += 8;
                    continue;
                }

                if mask == 0xFF {
                    // This point has 8 adjacencies.
                    // No need to check them individually.
                    passable_count += 8;
                    continue;
                }

                // Check all the adjacencies individually.
                for a in 0..8usize {
                    let nx = x as f32 + DX[a] as f32;
                    let nz = z as f32 + DZ[a] as f32;

                    // If the point isn't in this node, then we're
                    // crossing an existing boundary, so there's no need
                    // to try to split here.
                    if !self.bsp[index].point_in_node_f(nx, nz) {
                        continue;
                    }

                    // See if we cannot go at angle a.
                    if mask & (1 << a) == 0 {
                        passable = false;
                        impassable_count += 1;

                        // This edge should be passable, but it is not.
                        // So, if the edge to the left or right of it
                        // is passable, attempt to split the node along
                        // the passable edge.
                        if mask & (1 << ((a + 1) % 8)) != 0 {
                            let mut split = SplitDef {
                                normal: ((a as i32) + 7) % 8,
                                value: -1.0,
                                ph: [x as f32, z as f32],
                            };
                            self.calc_split_value(&self.bsp[index], &mut split);
                            if split.value > best_split.value {
                                best_split = split;
                            }
                        }

                        if mask & (1 << ((a + 7) % 8)) != 0 {
                            let mut split = SplitDef {
                                normal: ((a as i32) + 1) % 8,
                                value: -1.0,
                                ph: [x as f32, z as f32],
                            };
                            self.calc_split_value(&self.bsp[index], &mut split);
                            if split.value > best_split.value {
                                best_split = split;
                            }
                        }
                    } else {
                        passable_count += 1;
                    }
                }
            }
        }

        // If after all that, we didn't find any impassable edges,
        // this node is entirely passable. So classify it as a waypoint.
        if best_split.value == -1.0 && passable_count > impassable_count {
            passable = true;
        }

        // If there is too much variation in height and we're not going
        // to split it for another reason, then split away
        if passable && y2 - y1 > MAX_HEIGHT_RANGE {
            passable = false;

            if best_split.value == -1.0 {
                best_split.normal = VERTICAL_SPLIT;
                best_split.ph = [y1, y2];
                best_split.value = 1.0;
            }
        }

        // If it's still considered passable, flag it as a waypoint and
        // return
        if passable {
            self.bsp[index].waypoint = true;
            return;
        }

        // If we have no idea how to split this node, then give up
        // (whether or not it was passable, usually mostly impassable)
        if best_split.value <= 0.0 {
            self.bsp[index].waypoint = false;
            return;
        }

        // If our node is really big, replace whatever split was selected
        // with an even split, so a more balanced tree is created.
        let width = (x2 - x1) as f32;
        let height = (z2 - z1) as f32;
        if width > EVEN_SPLIT_THRESHOLD || height > EVEN_SPLIT_THRESHOLD {
            best_split.ph = [
                (x1 + (width / 2.0) as i32) as f32,
                (z1 + (height / 2.0) as i32) as f32,
            ];
            best_split.normal = if width > height { 2 } else { 0 };
        }

        // And subdivide it further
        if self.split_node(index, &best_split, false) {
            let back = self.bsp[index].back as usize;
            let front = self.bsp[index].front as usize;
            index_stack.push(back);
            index_stack.push(front);
        } else {
            // the split was vertical and there was a problem.
            // if there was a problem, bsp_ and indexStack are untouched.
            self.do_best_horizontal_split(index, index_stack);
        }
    }

    /// `doBestHorizontalSplit` (`waypoint_generator.cpp:697-797`).
    fn do_best_horizontal_split(&mut self, index: usize, index_stack: &mut Vec<usize>) {
        let mut override_children = false;

        // find centre
        self.bsp[index].calc_centre();
        let mut node = std::mem::take(&mut self.bsp[index]);
        self.reduce_node_height(&mut node);
        self.bsp[index] = node;

        let bx2 = -self.bsp[index].border_offset[2];
        let bz0 = -self.bsp[index].border_offset[0];
        let cx = (self.bsp[index].centre.0 + bx2).floor();
        let cz = (self.bsp[index].centre.1 + bz0).floor();

        let mut split = SplitDef {
            normal: 0,
            value: -1.0,
            ph: [cx, cz],
        };
        let mut best_split = split;

        // don't override existing bsp_ children on first split - create
        // new ones.
        if self.split_node(index, &split, override_children) {
            override_children = true;

            let front = self.bsp[index].front as usize;
            let back = self.bsp[index].back as usize;
            let hgt1 = self.average_height(front);
            let hgt2 = self.average_height(back);
            // ensure at least some points in each node.
            if hgt1.1 != 0 && hgt2.1 != 0 {
                // ensure evenish split.
                let ratio = hgt1.1 as f32 / hgt2.1 as f32;
                if (0.2..=5.0).contains(&ratio) {
                    // acceptable split - store!
                    best_split.value = (hgt1.0 - hgt2.0).abs();
                }
            }
        }

        // trying latter 4 angles is redundant.
        for a in 1..4i32 {
            split.normal = a;
            // override already calculated front/backs.
            if self.split_node(index, &split, override_children) {
                override_children = true;

                let front = self.bsp[index].front as usize;
                let back = self.bsp[index].back as usize;
                let hgt1 = self.average_height(front);
                let hgt2 = self.average_height(back);
                // ensure at least some points in each node.
                if hgt1.1 != 0 && hgt2.1 != 0 {
                    // ensure evenish split.
                    let ratio = hgt1.1 as f32 / hgt2.1 as f32;
                    if (0.2..=5.0).contains(&ratio) {
                        let value = (hgt1.0 - hgt2.0).abs();
                        // if best split so far, then store!
                        if value > best_split.value {
                            best_split.value = value;
                            best_split.normal = a;
                        }
                    }
                }
            }
        }

        if best_split.value >= 0.0 {
            // split it!
            self.split_node(index, &best_split, true);

            // check to see if front or back are the same
            let front = self.bsp[index].front as usize;
            let back = self.bsp[index].back as usize;
            let mut same = self.nodes_same(index, front);

            if !same {
                same = self.nodes_same(index, back);
                if !same {
                    index_stack.push(back);
                    index_stack.push(front);
                    return;
                }
            }
        }

        // split didn't work! quit. not waypoints, and don't add to the
        // stack to recurse.
        let back = self.bsp[index].back as usize;
        let front = self.bsp[index].front as usize;
        self.bsp[back].waypoint = false;
        self.bsp[front].waypoint = false;
    }

    /// `averageHeight` (`waypoint_generator.cpp:800-861`): the mean
    /// height of the most populated level, with its point count.
    fn average_height(&self, node_index: usize) -> (f32, i32) {
        let node = &self.bsp[node_index];
        let z1 = (-node.border_offset[0]) as i32;
        let z2 = node.border_offset[4] as i32;
        let x1 = (-node.border_offset[2]) as i32;
        let x2 = node.border_offset[6] as i32;
        let y1 = node.min_height;
        let y2 = node.max_height;

        let mut sums = [0.0f32; MAX_HEIGHTS];
        let mut counts = [0i32; MAX_HEIGHTS];

        for x in x1..=x2 {
            for z in z1..=z2 {
                if !node.point_in_node_f(x as f32, z as f32) {
                    continue;
                }
                let ptindex = (x + self.grid_x as i32 * z) as usize;
                for g in 0..MAX_HEIGHTS {
                    let h = self.hgt_grids[g][ptindex];
                    if h < y1 || h > y2 {
                        continue;
                    }
                    if self.adj_grids[g][ptindex].all == 0 {
                        continue;
                    }
                    sums[g] += h;
                    counts[g] += 1;
                }
            }
        }

        // find highest count. we will split based on height diff in the
        // most populated level.
        let mut maxs = 0f32;
        let mut maxc = 0i32;
        for i in 0..MAX_HEIGHTS {
            if maxc < counts[i] {
                maxc = counts[i];
                maxs = sums[i];
            }
        }

        if maxc == 0 {
            return (0.0, 0);
        }

        (maxs / maxc as f32, maxc)
    }

    /// `findDispoints` (`waypoint_generator.cpp:869-929`): whether the
    /// node's height range disconnects a point from an in-node
    /// neighbour. The reference refreshes the node flags here too; the
    /// port relies on `reduceNodeHeight` having run immediately before
    /// (it owns the same refresh).
    fn find_dispoints(&self, front_node: &BspNode) -> bool {
        let tgx = self.grid_x as i32;
        let dx = [0, 1, 1, 1, 0, -1, -1, -1];
        let dy = [tgx, tgx, 0, -tgx, -tgx, -tgx, 0, tgx];

        let z1 = (-front_node.border_offset[0]) as i32;
        let z2 = front_node.border_offset[4] as i32;
        let x1 = (-front_node.border_offset[2]) as i32;
        let x2 = front_node.border_offset[6] as i32;

        for x in x1..=x2 {
            for z in z1..=z2 {
                if !front_node.point_in_node_i(x, z) {
                    continue;
                }

                let ptindex = (x + self.grid_x as i32 * z) as usize;

                for g in 0..MAX_HEIGHTS {
                    let ah = self.hgt_grids[g][ptindex];
                    if ah < front_node.min_height || ah > front_node.max_height {
                        continue;
                    }

                    let age = &self.adj_grids[g][ptindex];
                    if age.all == 0 {
                        continue;
                    }
                    for a in 0..8usize {
                        let og = age.angle(a);
                        if og == 0 {
                            continue;
                        }

                        if !front_node.point_in_node_i(x + DX[a], z + DZ[a]) {
                            continue;
                        }

                        // ok, we want to be connected to this node, and
                        // it is within our x-z range, so check its height
                        let h =
                            self.hgt_grids[og as usize][(ptindex as i32 + dx[a] + dy[a]) as usize];
                        if h >= front_node.min_height && h <= front_node.max_height {
                            continue;
                        }

                        return true;
                    }
                }
            }
        }

        false
    }

    /// `calcSplitValue` (`waypoint_generator.cpp:938-984`): the larger
    /// this value, the better the split; currently the inverse distance
    /// from the centre of the node.
    fn calc_split_value(&self, node: &BspNode, split: &mut SplitDef) {
        // If the normal is 8, then we're doing a vertical split.
        // Calculate value using an algo similar to a horizontal split
        // (inverse of distance from centre squared).
        if split.normal == VERTICAL_SPLIT {
            let mut value =
                (split.ph[0] + split.ph[1]) * 0.5 - (node.min_height + node.max_height) * 0.5;
            value *= value;
            if value == 0.0 {
                value = 0.1;
            }
            split.value = 1.0 / value;
            return;
        }

        // First check to see if the split is valid. If not, return a
        // value of -1.
        let n = split.normal as usize;
        let n1 = node.border_offset[n];
        let n2 = -dot2(normal(n), (split.ph[0], split.ph[1]));

        let n3 = node.border_offset[n ^ 4];
        let n4 = -dot2(normal(n ^ 4), (split.ph[0], split.ph[1]));

        if (n1 - n2).abs() < 0.001 || (n3 - n4).abs() < 0.001 {
            split.value = -1.0;
            return;
        }

        // Split value is currently the inverse of the distance from the
        // centre of the node. A larger split value is better, and they
        // must be greater than zero to be valid.
        let dx = node.centre.0 - split.ph[0];
        let dz = node.centre.1 - split.ph[1];
        let mut value = dx * dx + dz * dz;
        if value < 0.1 {
            value = 0.1;
        }

        split.value = 1.0 / value;
    }

    /// `splitNode` (`waypoint_generator.cpp:992-1131`): creates two new
    /// BSP nodes and calculates the new bounding edges for each one.
    /// Returns false only on a vertical split problem (bsp_ untouched).
    fn split_node(&mut self, index: usize, split: &SplitDef, replace: bool) -> bool {
        let node = self.bsp[index].clone();
        let mut front = BspNode {
            parent: index as i32,
            front: -1,
            back: -1,
            ..node.clone()
        };
        let mut back = BspNode {
            parent: index as i32,
            front: -1,
            back: -1,
            ..node.clone()
        };

        let d1;
        let mut d2;
        if split.normal == VERTICAL_SPLIT {
            if split.ph[0] == split.ph[1] {
                // cannot do a vertical split, heights are the same
                return false;
            }
            d1 = (split.ph[0] + split.ph[1]) * 0.5;
            d2 = d1;
        } else {
            d1 = -dot2(normal(split.normal as usize), (split.ph[0], split.ph[1]));
            d2 = -d1;
        }

        let mut node = BspNode {
            split_normal: split.normal,
            split_offset: d1,
            ..node
        };

        if split.normal == VERTICAL_SPLIT {
            front.max_height = d1;
        } else {
            front.border_offset[split.normal as usize] = d1;
        }

        self.refine_node_edges(&mut front);
        front.calc_centre();
        self.reduce_node_height(&mut front);

        if split.normal == VERTICAL_SPLIT && self.find_dispoints(&front) {
            // Vertical split at d1 leads to points disconnected by
            // split; walk the front's ceiling up until they connect.
            let mut h = split.ph[0] + 0.1;
            while h < split.ph[1] - 0.1 {
                front.max_height = h;

                self.refine_node_edges(&mut front);
                front.calc_centre();
                self.reduce_node_height(&mut front);
                if !self.find_dispoints(&front) {
                    d2 = h;
                    break;
                }
                h += 0.1;
            }

            if h >= split.ph[1] - 0.1 {
                // problem with vertical split. bsp_[index] unchanged.
                return false;
            }
        }

        if split.normal == VERTICAL_SPLIT {
            // split nodes must share some points!
            back.min_height = d2;
        } else {
            back.border_offset[(split.normal ^ 4) as usize] = d2;
        }

        self.refine_node_edges(&mut back);
        back.calc_centre();
        self.reduce_node_height(&mut back);

        let zero_size = |o: &[f32; 8]| {
            (o[0] + o[4]).abs() < 0.01
                || (o[1] + o[5]).abs() < 0.01
                || (o[2] + o[6]).abs() < 0.01
                || (o[3] + o[7]).abs() < 0.01
        };
        if zero_size(&back.border_offset) || zero_size(&front.border_offset) {
            return false;
        }

        if replace {
            let front_index = node.front as usize;
            let back_index = node.back as usize;
            self.bsp[front_index] = front;
            self.bsp[back_index] = back;
        } else {
            node.front = self.bsp.len() as i32;
            node.back = (self.bsp.len() + 1) as i32;
            self.bsp.push(front);
            self.bsp.push(back);
        }

        self.bsp[index] = node; // change node only if split.
        true
    }

    /// `pointInNodeDiagonalOnly`
    /// (`waypoint_generator.cpp:1136-1166`): the offsets are already
    /// divided by `HALF_ROOT_2`.
    fn point_in_node_diagonal_only(x: f32, z: f32, offset: &[f32; 4]) -> bool {
        if x + z + offset[0] < -0.1 {
            return false;
        }
        if x - z + offset[1] < -0.1 {
            return false;
        }
        if -x - z + offset[2] < -0.1 {
            return false;
        }
        if -x + z + offset[3] < -0.1 {
            return false;
        }
        true
    }

    /// `refineNodeEdges` (`waypoint_generator.cpp:1171-1225`): refine
    /// the node edges to make them exactly wrap the BSP node.
    fn refine_node_edges(&self, node: &mut BspNode) {
        let z1 = -node.border_offset[0];
        let z2 = node.border_offset[4];
        let x1 = -node.border_offset[2];
        let x2 = node.border_offset[6];

        let offsets = [
            node.border_offset[1] / HALF_ROOT_2,
            node.border_offset[3] / HALF_ROOT_2,
            node.border_offset[5] / HALF_ROOT_2,
            node.border_offset[7] / HALF_ROOT_2,
        ];

        let mut min_x = 100000f32;
        let mut max_x = 0f32;
        let mut min_z = 100000f32;
        let mut max_z = 0f32;

        let mut x = x1;
        while x <= x2 {
            let mut z = z1;
            while z <= z2 {
                if x >= min_x && x <= max_x && z >= min_z && z <= max_z {
                    z += self.grid_resolution;
                    continue;
                }

                if !Self::point_in_node_diagonal_only(x, z, &offsets) {
                    z += self.grid_resolution;
                    continue;
                }
                if x < min_x {
                    min_x = x;
                }
                if x > max_x {
                    max_x = x;
                }
                if z < min_z {
                    min_z = z;
                }
                if z > max_z {
                    max_z = z;
                }
                z += self.grid_resolution;
            }
            x += self.grid_resolution;
        }

        if min_x > max_x || min_z > max_z {
            min_x = x1;
            max_x = x1;
            min_z = z1;
            max_z = z1;
        }

        // prevent the BSP node from having zero size
        if almost_equal(min_x, max_x) {
            if max_x < x2 {
                max_x += self.grid_resolution;
            } else {
                min_x -= self.grid_resolution;
            }
        }
        if almost_equal(min_z, max_z) {
            if max_z < z2 {
                max_z += self.grid_resolution;
            } else {
                min_z -= self.grid_resolution;
            }
        }

        node.border_offset[0] = -min_z;
        node.border_offset[4] = max_z;
        node.border_offset[2] = -min_x;
        node.border_offset[6] = max_x;
    }

    /// `reduceNodeHeight` (`waypoint_generator.cpp:1229-1272`): clamp
    /// the node's height range to the grid heights it actually
    /// contains.
    fn reduce_node_height(&mut self, node: &mut BspNode) {
        let mut new_min = 13000f32;
        let mut new_max = -13000f32;

        let z1 = (-node.border_offset[0]) as i32;
        let z2 = node.border_offset[4] as i32;
        let x1 = (-node.border_offset[2]) as i32;
        let x2 = node.border_offset[6] as i32;

        node.update_point_in_node_flags();

        for x in x1..=x2 {
            for z in z1..=z2 {
                if !node.point_in_node_i(x, z) {
                    continue;
                }

                let ptindex = (x + self.grid_x as i32 * z) as usize;
                for g in (0..MAX_HEIGHTS).rev() {
                    let height = self.hgt_grids[g][ptindex];
                    if height <= -9999.0 {
                        break;
                    }
                    if height < node.min_height || height > node.max_height {
                        continue;
                    }
                    // also ignore this point if it has no adjacencies
                    // (this is so failed fills do not distract us)
                    if self.adj_grids[g][ptindex].all == 0 {
                        continue;
                    }

                    if new_max < height {
                        new_max = height;
                    }
                    if new_min > height {
                        new_min = height;
                    }
                }
            }
        }

        node.min_height = new_min;
        node.max_height = new_max;
    }

    /// `calcEdge`'s two-point form (`waypoint_generator.cpp:1400-1460`):
    /// the edge's start and end `PointDef`s, or `None` when the edge
    /// does not exist (`t1 <= t2`).
    fn calc_edge_pair(&self, index: usize, edge: usize) -> (bool, PointDef, PointDef) {
        let node = &self.bsp[index];

        // The starting point is either the intersection with the x or z
        // axis, depending on the gradient of the line.
        let s = if normal(edge).0 == 0.0 {
            (0.0, -node.border_offset[edge] / normal(edge).1)
        } else {
            (-node.border_offset[edge] / normal(edge).0, 0.0)
        };

        let d = normal((edge + 6) % 8);

        let mut t1 = 0f32;
        for i in 1..=3usize {
            let edge2 = (edge + i) % 8;
            let n = normal(edge2);
            let big_d = -node.border_offset[edge2];
            let t = (big_d - dot2(n, s)) / dot2(n, d);
            if i == 1 || t < t1 {
                t1 = t;
            }
        }

        let mut t2 = 0f32;
        for i in 5..=7usize {
            let edge2 = (edge + i) % 8;
            let n = normal(edge2);
            let big_d = -node.border_offset[edge2];
            let t = (big_d - dot2(n, s)) / dot2(n, d);
            if i == 5 || t > t2 {
                t2 = t;
            }
        }

        if t1 <= t2 {
            return (false, PointDef::default(), PointDef::default());
        }

        let p1 = PointDef {
            angle: edge as i32,
            offset: node.border_offset[edge],
            t: t1,
            min_height: node.min_height,
            max_height: node.max_height,
        };

        let p2 = PointDef {
            angle: edge as i32,
            offset: node.border_offset[edge],
            t: t2,
            min_height: node.min_height,
            max_height: node.max_height,
        };

        (true, p1, p2)
    }

    /// `generatePoints` (`waypoint_generator.cpp:1467-1517`).
    fn generate_points(&mut self) {
        // insert an invalid point so we never need to look at
        // points_.begin
        let sentinel = PointDef {
            angle: -1000,
            ..PointDef::default()
        };
        self.points.insert(sentinel);

        for i in 0..self.bsp.len() {
            if self.bsp[i].waypoint {
                for a in 0..4usize {
                    let (ok, p1, p2) = self.calc_edge_pair(i, a);
                    if ok {
                        self.points.insert(p1);
                        self.points.insert(p2);
                    }
                }

                // The last 4 edges use the same angles as the first.
                for a in 4..8usize {
                    let (ok, mut p1, mut p2) = self.calc_edge_pair(i, a);
                    if ok {
                        self.invert_point(&mut p1);
                        self.invert_point(&mut p2);
                        self.points.insert(p1);
                        self.points.insert(p2);
                    }
                }
            }
        }
    }

    /// `invertPoint` (`waypoint_generator.cpp:1522-1529`): takes a
    /// `PointDef` for an angle 4-8 and inverts it such that the angle
    /// is 0-3.
    fn invert_point(&self, p: &mut PointDef) {
        p.angle -= 4;
        p.offset = -p.offset;
        p.t = -p.t;
    }

    /// `pointToVertex` (`waypoint_generator.cpp:1551-1580`), including
    /// `roundToFraction` rounding to the nearest 0.5: two diagonals can
    /// intersect halfway between gridpoints, so 0.5 accuracy is needed.
    fn point_to_vertex(&self, p: &PointDef) -> (f32, f32) {
        let n = normal(p.angle as usize);
        let s = if n.0 == 0.0 {
            (0.0, -p.offset / n.1)
        } else {
            (-p.offset / n.0, 0.0)
        };

        let d = normal((p.angle as usize + 6) % 8);
        let vx = s.0 + d.0 * p.t;
        let vy = s.1 + d.1 * p.t;

        (round_to_fraction(vx, 2), round_to_fraction(vy, 2))
    }

    /// `generatePolygons` (`waypoint_generator.cpp:1592-1700`).
    fn generate_polygons(&mut self) {
        // The point set is const during this pass; snapshot it in the
        // set's order so the lower_bound/upper_bound walks are index
        // based (BTreeSet iterates sorted, like BW::set).
        let points: Vec<PointDef> = self.points.iter().copied().collect();
        let point_count = points.len();

        for i in 0..self.bsp.len() {
            if self.bsp[i].waypoint {
                // make sure we have a polygon with area
                let bo = self.bsp[i].border_offset;
                if bo[0] == -bo[4] || bo[1] == -bo[5] || bo[2] == -bo[6] || bo[3] == -bo[7] {
                    continue;
                }

                let mut poly_def = PolygonDef {
                    vertices: Vec::new(),
                    min_height: 0.0,
                    max_height: 0.0,
                    set: 0,
                };

                for a in 0..4usize {
                    let (ok, p1, mut p2) = self.calc_edge_pair(i, a);
                    if ok {
                        p2.min_height = -13000.0; // sort before all other points
                        let mut it = points.partition_point(|x| x < &p2);
                        while it < point_count && !points[it].eq_ref(&p1) {
                            let point = points[it];
                            // make sure it overlaps us
                            if point.max_height < p1.min_height || point.min_height > p1.max_height
                            {
                                it += 1;
                                continue;
                            }

                            // make a vertex out of it
                            let v = self.point_to_vertex(&point);
                            poly_def.vertices.push(VertexDef {
                                pos: v,
                                angles: (a as i32 - 2) % 8,
                                adj_nav_poly: 0,
                                adj_to_another_chunk: false,
                            });
                            it += 1;
                        }
                    }
                }

                for a in 4..8usize {
                    let (ok, mut p1, mut p2) = self.calc_edge_pair(i, a);
                    if ok {
                        self.invert_point(&mut p1);
                        self.invert_point(&mut p2);

                        p2.min_height = 13000.0; // sort after all other points
                        let upper = points.partition_point(|x| x <= &p2);
                        let mut it = upper.saturating_sub(1);
                        while it > 0 && !points[it].eq_ref(&p1) {
                            let point = points[it];
                            if point.max_height < p1.min_height || point.min_height > p1.max_height
                            {
                                it -= 1;
                                continue;
                            }

                            let v = self.point_to_vertex(&point);
                            poly_def.vertices.push(VertexDef {
                                pos: v,
                                angles: (a as i32 - 2) % 8,
                                adj_nav_poly: 0,
                                adj_to_another_chunk: false,
                            });
                            it -= 1;
                        }
                    }
                }

                if !poly_def.vertices.is_empty() {
                    poly_def.min_height = self.bsp[i].min_height;
                    poly_def.max_height = self.bsp[i].max_height;
                    poly_def.set = 0;
                    self.bsp[i].waypoint_index = self.polygons.len() as i32;
                    self.polygons.push(poly_def);
                }
            }
        }
    }

    /// `generateAdjacencies` (`waypoint_generator.cpp:1705-1782`):
    /// first add all the edges, then for each polygon edge search for
    /// the reverse edge and adopt its polygon id.
    fn generate_adjacencies(&mut self) {
        for p in 0..self.polygons.len() {
            let vertex_count = self.polygons[p].vertices.len();
            for v in 0..vertex_count {
                let from = self.polygons[p].vertices[v].pos;
                let to = self.polygons[p].vertices[(v + 1) % vertex_count].pos;
                if from == to {
                    continue; // skip zero-length edges
                }
                self.edges.insert(EdgeDef {
                    from,
                    to,
                    id: p as i32 + 1,
                });
            }
        }

        // polygon heights and the edge set are const across this pass
        let heights: Vec<(f32, f32)> = self
            .polygons
            .iter()
            .map(|poly| (poly.min_height, poly.max_height))
            .collect();
        let edges: Vec<EdgeDef> = self.edges.iter().copied().collect();

        // Now calculate adjacencies.
        for p in 0..self.polygons.len() {
            let vertex_count = self.polygons[p].vertices.len();
            for v in 0..vertex_count {
                self.polygons[p].vertices[v].adj_nav_poly = 0;

                let to = self.polygons[p].vertices[v].pos;
                let from = self.polygons[p].vertices[(v + 1) % vertex_count].pos;
                let edge = EdgeDef { from, to, id: -1 };

                // make sure our edges overlap in 3D
                let mut it =
                    edges.partition_point(|x| x.cmp_ref(&edge) == std::cmp::Ordering::Less);
                while it < edges.len() && edges[it].eq_ref(&edge) {
                    let maybe_adj = (edges[it].id - 1) as usize;
                    if heights[maybe_adj].1 < heights[p].0 || heights[maybe_adj].0 > heights[p].1 {
                        it += 1;
                        continue;
                    }
                    self.polygons[p].vertices[v].adj_nav_poly = edges[it].id;
                    break;
                }
            }
        }
    }

    /// `isConvexJoint` (`waypoint_generator.cpp:2064-2074`).
    fn is_convex_joint(a1: i32, a2: i32, b: i32) -> bool {
        if a1 < a2 {
            (b >= a1) && (b < a2)
        } else if a2 < a1 {
            (b >= a1) || (b < a2)
        } else {
            false
        }
    }

    /// `findPolygonVertex` (`waypoint_generator.cpp:2081-2097`).
    fn find_polygon_vertex(&self, poly_index: usize, v: (f32, f32)) -> i32 {
        for (i, vertex) in self.polygons[poly_index].vertices.iter().enumerate() {
            if (v.0 - vertex.pos.0).abs() < GENERATOR_FLOAT_FUDGE
                && (v.1 - vertex.pos.1).abs() < GENERATOR_FLOAT_FUDGE
            {
                return i as i32;
            }
        }
        -1
    }

    /// `fixupAdjacencies` (`waypoint_generator.cpp:2110-2121`): after
    /// two polygons have been joined, all of the loser's adjacencies
    /// need to link to the winner instead.
    fn fixup_adjacencies(&mut self, poly_index: usize, old_adj: i32, new_adj: i32) {
        for vertex in &mut self.polygons[poly_index].vertices {
            if vertex.adj_nav_poly == old_adj {
                vertex.adj_nav_poly = new_adj;
            }
        }
    }

    /// `joinPolygon` (`waypoint_generator.cpp:2129-2267`): attempts to
    /// join a polygon to one of its neighbours.
    fn join_polygon(&mut self, poly_index: usize) -> bool {
        let i1 = poly_index;
        if self.polygons[i1].vertices.is_empty() {
            // Check for empty polygons, they can exist.
            return false;
        }
        let size1 = self.polygons[i1].vertices.len() as i32;

        // Find the first vertex with a different angle. This is to make
        // sure we start at the beginning of a new edge.
        let angle0 = self.polygons[i1].vertices[0].angles;
        let mut first_vertex = 1i32;
        while first_vertex < size1
            && self.polygons[i1].vertices[first_vertex as usize].angles == angle0
        {
            first_vertex += 1;
        }
        assert!(
            first_vertex < size1,
            "WaypointGenerator::joinPolygon: single-angle polygon"
        );

        let mut edge_start1 = first_vertex;
        let mut edge_end1 = first_vertex;

        loop {
            // To consider an edge for joining, every point on it must be
            // the same angle (obviously), and they all must have the
            // same adjacent edge.
            let mut single_adjacency =
                self.polygons[i1].vertices[edge_start1 as usize].adj_nav_poly > 0;

            // Keep going until we reach a vertex with a different angle.
            while self.polygons[i1].vertices[edge_start1 as usize].angles
                == self.polygons[i1].vertices[edge_end1 as usize].angles
            {
                if self.polygons[i1].vertices[edge_start1 as usize].adj_nav_poly
                    != self.polygons[i1].vertices[edge_end1 as usize].adj_nav_poly
                {
                    single_adjacency = false;
                }
                edge_end1 = (edge_end1 + 1) % size1;
            }

            // Now we have a complete edge, (edgeStart <= v < edgeEnd).
            // If all points on it link to the same adjacency, we can
            // consider linking it.
            if single_adjacency {
                let i2 =
                    (self.polygons[i1].vertices[edge_start1 as usize].adj_nav_poly - 1) as usize;
                assert!(i2 < self.polygons.len());
                let size2 = self.polygons[i2].vertices.len() as i32;

                let end_pos = self.polygons[i1].vertices[edge_end1 as usize].pos;
                let start_pos = self.polygons[i1].vertices[edge_start1 as usize].pos;
                let edge_start2 = self.find_polygon_vertex(i2, end_pos);
                let edge_end2 = self.find_polygon_vertex(i2, start_pos);

                if size2 <= 0 {
                    // (the reference logs an error here and skips)
                } else if edge_start2 == -1 || edge_end2 == -1 {
                    // (likewise: couldn't find the edge vertices)
                } else {
                    let edge_prev1 = (edge_start1 + size1 - 1) % size1;
                    let edge_prev2 = (edge_start2 + size2 - 1) % size2;

                    // also make sure they do not encompass too much
                    // height range
                    let combined_min_height = self.polygons[i1]
                        .min_height
                        .min(self.polygons[i2].min_height);
                    let combined_max_height = self.polygons[i1]
                        .max_height
                        .max(self.polygons[i2].max_height);

                    let convex1 = Self::is_convex_joint(
                        self.polygons[i1].vertices[edge_prev1 as usize].angles,
                        self.polygons[i1].vertices[edge_start1 as usize].angles,
                        self.polygons[i2].vertices[edge_end2 as usize].angles,
                    );
                    let convex2 = Self::is_convex_joint(
                        self.polygons[i2].vertices[edge_prev2 as usize].angles,
                        self.polygons[i2].vertices[edge_start2 as usize].angles,
                        self.polygons[i1].vertices[edge_end1 as usize].angles,
                    );

                    if convex1
                        && convex2
                        && combined_max_height - combined_min_height <= MAX_HEIGHT_RANGE
                    {
                        let mut p3: Vec<VertexDef> = Vec::new();

                        let p1_vertices = self.polygons[i1].vertices.clone();
                        let p2_vertices = self.polygons[i2].vertices.clone();

                        let mut i = edge_end1;
                        while i != edge_start1 {
                            p3.push(p1_vertices[i as usize]);
                            i = (i + 1) % size1;
                        }

                        let mut i = edge_end2;
                        while i != edge_start2 {
                            let vertex = p2_vertices[i as usize];
                            p3.push(vertex);
                            if vertex.adj_nav_poly > 0 {
                                self.fixup_adjacencies(
                                    (vertex.adj_nav_poly - 1) as usize,
                                    i2 as i32 + 1,
                                    i1 as i32 + 1,
                                );
                            }
                            i = (i + 1) % size2;
                        }

                        // fix up all the bsp indices too
                        for bsp_node in &mut self.bsp {
                            if bsp_node.waypoint_index == i2 as i32 {
                                bsp_node.waypoint_index = i1 as i32;
                            }
                        }

                        self.polygons[i1] = PolygonDef {
                            vertices: p3,
                            min_height: combined_min_height,
                            max_height: combined_max_height,
                            set: 0,
                        };
                        self.polygons[i2].vertices.clear();
                        return true;
                    }
                }
            }

            edge_start1 = edge_end1;
            if edge_end1 == first_vertex {
                break;
            }
        }

        false
    }

    /// `joinPolygons` (`waypoint_generator.cpp:2270-2295`): attempts to
    /// join as many polygons as possible to their neighbours.
    fn join_polygons(&mut self) {
        loop {
            let mut join_count = 0;
            for i in 0..self.polygons.len() {
                if self.join_polygon(i) {
                    join_count += 1;
                }
            }
            if join_count == 0 {
                break;
            }
        }
    }

    /// `streamline` (`waypoint_generator.cpp:2733-2818`): removes empty
    /// polygons, collapses doubled and zero-length adjacencies, and
    /// remaps the surviving ids.
    pub fn streamline(&mut self) {
        let mut id_remap: Vec<i32> = Vec::new();
        let mut new_id = 0i32;
        id_remap.push({
            let v = new_id;
            new_id += 1;
            v
        });
        let mut lost_edges = 0u32;

        // first remove empty polygons or collapse shared edges
        let mut p = 0usize;
        while p < self.polygons.len() {
            if self.polygons[p].vertices.is_empty() {
                id_remap.push(-1);
                self.polygons.remove(p);
                continue;
            }

            id_remap.push({
                let v = new_id;
                new_id += 1;
                v
            });

            // find doubled adjacencies
            let mut last_adj = self.polygons[p].vertices.last().unwrap().adj_nav_poly;
            let mut v = 0usize;
            while v < self.polygons[p].vertices.len() {
                let this_adj = self.polygons[p].vertices[v].adj_nav_poly;
                if this_adj == last_adj && this_adj > 0 {
                    // delete our side only (other side found by symmetry)
                    self.polygons[p].vertices.remove(v);
                    lost_edges += 1;
                } else {
                    v += 1;
                }
                last_adj = this_adj;
            }

            // find zero-length adjacencies
            let mut v = 0usize;
            while v + 1 < self.polygons[p].vertices.len() {
                let this_pt = self.polygons[p].vertices[v].pos;
                let next_pt = self.polygons[p].vertices[v + 1].pos;
                if next_pt == this_pt {
                    self.polygons[p].vertices.remove(v);
                } else {
                    v += 1;
                }
            }
            p += 1;
        }

        // now remap the adjacencies after the id space contraction
        self.remap_adjacencies(&id_remap);

        // (the reference warns when an odd number of edges was removed;
        // the count itself has no further effect, so it is not kept)
        let _ = lost_edges;
    }

    /// The shared adjacency-remap tail of `streamline` and
    /// `calculateSetMembership` (`waypoint_generator.cpp:2781-2800` and
    /// `:3005-3019`): contract 1-based polygon ids through `id_remap`.
    /// The reference logs an error on a dangling id (and the dev build
    /// asserts); the release build falls back to 0.
    fn remap_adjacencies(&mut self, id_remap: &[i32]) {
        for polygon in &mut self.polygons {
            for vertex in &mut polygon.vertices {
                let old_adj = vertex.adj_nav_poly;
                if old_adj > 0 {
                    let mut new_adj = id_remap[old_adj as usize];
                    if new_adj == -1 {
                        new_adj = 0;
                    }
                    vertex.adj_nav_poly = new_adj;
                }
            }
        }
    }

    /// `calculateSetMembership`
    /// (`waypoint_generator.cpp:2824-3040`): determines what set each
    /// polygon is in, and removes isolated polygons. Assumes all
    /// adjacencies are bi-directional.
    pub fn calculate_set_membership(&mut self, frontier_pts: &[Vector3], _chunk_name: &str) {
        // convert world-coord frontier pts into our local coords
        let local_fps: Vec<(f32, f32, f32)> = frontier_pts
            .iter()
            .map(|fit| {
                (
                    (fit.x - self.grid_min.x) / self.grid_resolution,
                    fit.y,
                    (fit.z - self.grid_min.z) / self.grid_resolution,
                )
            })
            .collect();

        let mut set_next = 0i32;

        // go through all the polygons looking for frontier ones; also
        // keep track of the biggest one in case we don't find any
        let mut p_max: Option<usize> = None;
        let mut area_max = 0f32;
        let mut p = 0usize;
        while p < self.polygons.len() {
            if self.polygons[p].set != 0 {
                p += 1;
                continue;
            }

            let mut frontier = false;
            let mut pmin = (10000f32, 10000f32);
            let mut pmax = (-10000f32, -10000f32);
            for vertex in &self.polygons[p].vertices {
                // is it frontier 'coz it leads to another chunk?
                if vertex.adj_to_another_chunk {
                    frontier = true;
                }
                if pmin.0 > vertex.pos.0 {
                    pmin.0 = vertex.pos.0;
                }
                if pmin.1 > vertex.pos.1 {
                    pmin.1 = vertex.pos.1;
                }
                if pmax.0 < vertex.pos.0 {
                    pmax.0 = vertex.pos.0;
                }
                if pmax.1 < vertex.pos.1 {
                    pmax.1 = vertex.pos.1;
                }
            }
            if !frontier {
                // is it frontier 'coz a frontier pt is in it?
                for fit in &local_fps {
                    if self.polygons[p].pt_near_enough(*fit) {
                        frontier = true;
                        break;
                    }
                }
            }

            let area = (pmax.0 - pmin.0) * (pmax.1 - pmin.1);
            if p_max.is_none() || area_max < area {
                p_max = Some(p);
                area_max = area;
            }

            if !frontier {
                if set_next > 0 || p < self.polygons.len() - 1 {
                    p += 1;
                    continue;
                }

                // Forming from biggest polygon because none are on the
                // frontier to other chunks
                p = p_max.expect("biggest polygon tracked");
            }

            set_next += 1;

            // do an adjacency search to find all those in the same set
            // as us
            let mut index_stack = vec![p];
            while let Some(idx) = index_stack.pop() {
                if self.polygons[idx].set != 0 {
                    // MF_ASSERT(set == setNext) in the reference (dev
                    // builds only)
                    continue;
                }
                self.polygons[idx].set = set_next;

                let adjs: Vec<i32> = self.polygons[idx]
                    .vertices
                    .iter()
                    .map(|v| v.adj_nav_poly)
                    .collect();
                for (v, aidx) in adjs.iter().enumerate() {
                    if *aidx > 0 {
                        let target = (*aidx - 1) as usize;
                        let target_set = self.polygons[target].set;
                        if target_set != 0 {
                            if target_set != set_next {
                                // unreciprocated connection
                                self.polygons[idx].vertices[v].adj_nav_poly = 0;
                            }
                            continue;
                        }
                        index_stack.push(target);
                    }
                }
            }

            // if this one's being processed because there were no
            // frontier polygons, then stop now; we only want one set
            // like that.
            if !frontier {
                break;
            }

            p += 1;
        }

        let mut id_remap: Vec<i32> = Vec::new();
        let mut new_id = 0i32;
        id_remap.push({
            let v = new_id;
            new_id += 1;
            v
        });

        // remove isolated polygons
        let mut p = 0usize;
        while p < self.polygons.len() {
            if self.polygons[p].set == 0 {
                id_remap.push(-1);
                self.polygons.remove(p);
                continue;
            }
            id_remap.push({
                let v = new_id;
                new_id += 1;
                v
            });
            p += 1;
        }

        // now remap the adjacencies after the id space contraction
        self.remap_adjacencies(&id_remap);

        self.sets = set_next;
    }
}

/// `roundToFraction` (`waypoint_generator.cpp:1546-1552`): rounds to
/// the nearest fraction 1/denominator.
fn round_to_fraction(f: f32, denominator: i32) -> f32 {
    let mut f = f * denominator as f32;
    f = if f > 0.0 {
        ((f + 0.5) as i32) as f32
    } else {
        ((f - 0.5) as i32) as f32
    };
    f / denominator as f32
}

/// `Vector2::dotProduct`.
fn dot2(a: (f32, f32), b: (f32, f32)) -> f32 {
    a.0 * b.0 + a.1 * b.1
}

impl NavView for WaypointGenerator {
    /// `getPolygonCount` (`waypoint_generator.cpp:1787-1790`).
    fn polygon_count(&self) -> i32 {
        self.polygons.len() as i32
    }

    /// `getGirth` (`waypoint_generator.hpp:75`): always 0, so the
    /// writer falls back to the workflow's default girth.
    fn girth(&self, _set: i32) -> f32 {
        0.0
    }

    /// `getSet` (`waypoint_generator.cpp:1935`).
    fn poly_set(&self, poly: i32) -> i32 {
        self.polygons[poly as usize].set
    }

    fn min_height(&self, poly: i32) -> f32 {
        self.polygons[poly as usize].min_height
    }

    fn max_height(&self, poly: i32) -> f32 {
        self.polygons[poly as usize].max_height
    }

    /// `getVertexCount` (`waypoint_generator.cpp:1907`).
    fn vertex_count(&self, poly: i32) -> i32 {
        self.polygons[poly as usize].vertices.len() as i32
    }

    fn first_vertex(&self, poly: i32) -> (f32, f32) {
        self.vertex_xy(poly, 0)
    }

    /// `getVertex` (`waypoint_generator.cpp:1942`): `VertexRef` scales
    /// the position by gridResolution and offsets by gridMin
    /// (`waypoint_view.hpp:110-118`).
    fn vertex(&self, poly: i32, vertex: i32) -> NavVertex {
        let vd = &self.polygons[poly as usize].vertices[vertex as usize];
        let (x, z) = self.vertex_xy(poly, vertex);
        NavVertex {
            x,
            z,
            adj_nav_poly: vd.adj_nav_poly,
            adj_to_another_chunk: vd.adj_to_another_chunk,
        }
    }
}

impl WaypointGenerator {
    /// World-coordinate position of one vertex, as `VertexRef` sees it.
    /// The reference reads vertex 0 of vertex-less polygons through
    /// `PolygonRef` (release-build UB) and discards the value; the
    /// port returns the origin instead.
    fn vertex_xy(&self, poly: i32, vertex: i32) -> (f32, f32) {
        let vertices = &self.polygons[poly as usize].vertices;
        if vertices.is_empty() {
            return (0.0, 0.0);
        }
        let pos = vertices[vertex as usize].pos;
        (
            pos.0 * self.grid_resolution + self.grid_min.x,
            pos.1 * self.grid_resolution + self.grid_min.z,
        )
    }
}

#[cfg(test)]
mod artifacts;
#[cfg(test)]
mod roundtrip;
