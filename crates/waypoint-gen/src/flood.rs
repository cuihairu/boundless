//! `WaypointFlood` (`lib/waypoint_generator/waypoint_flood.{hpp,cpp}`).
//!
//! A flood fill over a collision scene sampled at a fixed resolution.
//! Every grid point holds up to 16 height layers; each layer holds an
//! [`AdjGridElt`] whose eight 4-bit slots record, per direction, the
//! height layer of the walkable neighbour (0 = no link).

use std::collections::BTreeSet;

use foundation::math::vector::Vector3;

/// `MAX_HEIGHTS` (`waypoint_flood.hpp:117`).
pub const MAX_HEIGHTS: usize = 16;

/// `DROP_FUDGE` (`waypoint_flood.hpp:39`).
pub const DROP_FUDGE: f32 = 0.1;
/// `FLOOD_FLOAT_FUDGE` (`waypoint_flood.cpp:85`).
const FLOOD_FLOAT_FUDGE: f32 = 0.01;
/// `HEIGHT_THRESHOLD` (`waypoint_flood.cpp:86`).
const HEIGHT_THRESHOLD: f32 = 1.0;
/// `INITIAL_HEIGHT` (`waypoint_flood.cpp:87`).
const INITIAL_HEIGHT: f32 = -10000.0;
/// `HACK_OFFSET` (`waypoint_flood.cpp:93`): all points are offset so the
/// movement checks never land exactly on polygon seams.
const HACK_OFFSET: f32 = 0.1;

/// `cstdmf::almostEqual` with the default 0.0004 epsilon.
fn almost_equal(a: f32, b: f32) -> bool {
    (a - b).abs() < 0.0004
}

/// The per-cell adjacency element (`waypoint_flood.hpp:15-37`). The
/// reference is a union of `all` with eight named 4-bit fields; the port
/// keeps `all` and indexes the same slots through [`AdjGridElt::angle`].
/// A slot stores the height layer (1..15) of the walkable neighbour in
/// that direction, 0 when there is none.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AdjGridElt {
    pub all: u32,
}

impl AdjGridElt {
    /// `angle(a)` (`waypoint_flood.hpp:18`).
    pub fn angle(&self, a: usize) -> u32 {
        (self.all >> (a << 2)) & 15
    }

    /// `angle(a, adj)` (`waypoint_flood.hpp:22`).
    pub fn set_angle(&mut self, a: usize, adj: u32) {
        self.all = (self.all & !(15 << (a << 2))) | (adj << (a << 2));
    }
}

/// `IPhysics` (`waypoint_flood.hpp:41-77`): the collision-scene seam. The
/// reference implementation is `tools/common/physics_handler.cpp`, which
/// is bound to the chunk collision scene; tests provide synthetic height
/// fields instead.
pub trait FloodPhysics {
    /// `getGirth`.
    fn get_girth(&self) -> Vector3;

    /// `getScrambleHeight`.
    fn get_scramble_height(&self) -> f32;

    /// `findDropPoint`: drop from `pos` straight down, reporting the hit
    /// height in `y`; false when there is nothing below.
    fn find_drop_point(&mut self, pos: &Vector3, y: &mut f32) -> bool;

    /// `isUnblocked` (`waypoint_flood.hpp:46-74`): whether the column at
    /// `src` connects to a standing surface at height `another_y`.
    fn is_unblocked(&mut self, src: &Vector3, another_y: f32) -> bool {
        let mut y = 0.0;
        if almost_equal(src.y, another_y) {
            return true;
        }
        if src.y < another_y {
            // the drop starts above the other height: src stays
            // unblocked when the ground lands within the fudge of src
            return self
                .find_drop_point(&Vector3::new(src.x, another_y + DROP_FUDGE, src.z), &mut y)
                && y <= src.y + DROP_FUDGE;
        }
        if src.y > another_y {
            return self.find_drop_point(&Vector3::new(src.x, src.y + DROP_FUDGE, src.z), &mut y)
                && y <= another_y + DROP_FUDGE;
        }
        true
    }

    /// `adjustMove`: prevents a collision between `src` and `dst`,
    /// reporting the adjusted destination in `dst2`. An impossible move
    /// is signalled by the reference implementation with
    /// `dst2.y = -10000` (`physics_handler.cpp:328,337`).
    fn adjust_move(&mut self, src: &Vector3, dst: &Vector3, dst2: &mut Vector3);
}

/// The chunk seam. `owns` is `Chunk::owns` (`chunk.cpp:3474`); the four
/// bounds are the space's `minCoordX/maxCoordX/minCoordZ/maxCoordZ`
/// (`waypoint_flood.cpp:537-543`). The reference's chunk-overlapper arm
/// of `owns` is not ported (no overlapper object on this side yet).
pub trait FloodChunk {
    fn owns(&self, point: &Vector3) -> bool;
    fn min_coord_x(&self) -> f32;
    fn max_coord_x(&self) -> f32;
    fn min_coord_z(&self) -> f32;
    fn max_coord_z(&self) -> f32;
}

/// `WaypointFlood::IProgress` (`waypoint_flood.hpp:86-92`): return true
/// to stop the flood early.
pub trait FloodProgress {
    fn filled(&mut self, npoints: i32) -> bool;
}

/// `WaypointFlood` (`waypoint_flood.hpp:83-141`).
#[derive(Debug, Clone)]
pub struct WaypointFlood {
    min: Vector3,
    max: Vector3,
    resolution: f32,
    xsize: i32,
    zsize: i32,
    size: i32,
    /// One height grid per layer, each `size` entries.
    hgt_grids: Vec<Vec<f32>>,
    /// One adjacency grid per layer, each `size` entries.
    adj_grids: Vec<Vec<AdjGridElt>>,
    boundary_set: BTreeSet<(i32, i32)>,
    smallest_height: i32,
}

impl Default for WaypointFlood {
    /// `WaypointFlood::WaypointFlood` (`waypoint_flood.cpp:99-115`).
    fn default() -> Self {
        Self {
            min: Vector3::ZERO,
            max: Vector3::ZERO,
            resolution: 0.0,
            xsize: 0,
            zsize: 0,
            size: 0,
            hgt_grids: (0..MAX_HEIGHTS).map(|_| Vec::new()).collect(),
            adj_grids: (0..MAX_HEIGHTS).map(|_| Vec::new()).collect(),
            boundary_set: BTreeSet::new(),
            smallest_height: MAX_HEIGHTS as i32,
        }
    }
}

/// The eight direction offsets in cell steps (`waypoint_flood.cpp:321-322`).
const DX: [i32; 8] = [0, 1, 1, 1, 0, -1, -1, -1];
const DZ: [i32; 8] = [1, 1, 0, -1, -1, -1, 0, 1];

impl WaypointFlood {
    /// `setArea` (`waypoint_flood.cpp:149-209`). The grid extents are
    /// computed from the parameters before quantisation; `min`/`max` are
    /// quantised to resolution multiples and then nudged by
    /// [`HACK_OFFSET`], matching the reference statement order.
    pub fn set_area(&mut self, min: &Vector3, max: &Vector3, resolution: f32) {
        self.min = *min;
        self.max = *max;
        self.resolution = resolution;

        if !almost_equal(
            (self.min.x / resolution) as i32 as f32 * resolution,
            self.min.x,
        ) {
            self.min.x = (self.min.x / resolution) as i32 as f32 * resolution;
        }

        if !almost_equal(
            (self.min.z / resolution) as i32 as f32 * resolution,
            self.min.z,
        ) {
            self.min.z = (self.min.z / resolution) as i32 as f32 * resolution;
        }

        if !almost_equal(
            (self.max.x / resolution) as i32 as f32 * resolution,
            self.max.x,
        ) {
            self.max.x = ((self.max.x / resolution) as i32 + 1) as f32 * resolution;
        }

        if !almost_equal(
            (self.max.z / resolution) as i32 as f32 * resolution,
            self.max.z,
        ) {
            self.max.z = ((self.max.z / resolution) as i32 + 1) as f32 * resolution;
        }

        // sizes come from the un-quantised parameters, like the reference
        self.xsize = ((max.x - min.x) / resolution) as i32 + 1;
        self.zsize = ((max.z - min.z) / resolution) as i32 + 1;
        self.size = self.xsize * self.zsize;

        self.min.x += HACK_OFFSET;
        self.max.x += HACK_OFFSET;
        self.min.z += HACK_OFFSET;
        self.max.z += HACK_OFFSET;

        self.boundary_set.clear();
        for g in 0..MAX_HEIGHTS {
            self.adj_grids[g] = vec![AdjGridElt::default(); self.size as usize];
            self.hgt_grids[g] = vec![INITIAL_HEIGHT; self.size as usize];
        }
    }

    /// `min` after quantisation and the hack offset.
    pub fn min(&self) -> &Vector3 {
        &self.min
    }

    /// `max` after quantisation and the hack offset.
    pub fn max(&self) -> &Vector3 {
        &self.max
    }

    /// `resolution`.
    pub fn resolution(&self) -> f32 {
        self.resolution
    }

    /// `xsize`.
    pub fn xsize(&self) -> i32 {
        self.xsize
    }

    /// `zsize`.
    pub fn zsize(&self) -> i32 {
        self.zsize
    }

    /// `adjGrids`.
    pub fn adj_grids(&self) -> &[Vec<AdjGridElt>] {
        &self.adj_grids
    }

    /// Mutable layer access. The reference hands out
    /// `AdjGridElt **adjGrids()` (`waypoint_flood.hpp:114`) the same way;
    /// `WaypointGenerator` and the TGA reader mutate through it.
    pub fn adj_grids_mut(&mut self) -> &mut [Vec<AdjGridElt>] {
        &mut self.adj_grids
    }

    /// Mutable height-layer access (`hgtGrids`, `waypoint_flood.hpp:115`).
    pub fn hgt_grids_mut(&mut self) -> &mut [Vec<f32>] {
        &mut self.hgt_grids
    }

    /// The boundary points recorded by `fill` (`boundarySet_`): source
    /// cells whose outward move failed the chunk-bounds check.
    pub fn boundary_set(&self) -> &BTreeSet<(i32, i32)> {
        &self.boundary_set
    }

    /// `hgtGrids`.
    pub fn hgt_grids(&self) -> &[Vec<f32>] {
        &self.hgt_grids
    }

    /// `smallestHeight_`, exposed because `postfilteradd`/`postfilterremove`
    /// only sweep layers from it up.
    pub fn smallest_height(&self) -> i32 {
        self.smallest_height
    }

    /// `checkBounds` (`waypoint_flood.cpp:230-294`): both points must be
    /// ownable by the current chunk, allowing one grid square outside so
    /// neighbouring chunks overlap slightly.
    fn check_bounds(
        &self,
        src: &Vector3,
        dst: &Vector3,
        physics: &mut dyn FloodPhysics,
        chunk: &dyn FloodChunk,
    ) -> bool {
        let mut dst = *dst;
        dst.y = src.y;

        for i in 0..16 {
            let mut isrc = *src;
            let mut idst = dst;

            if i & 1 != 0 {
                isrc.x += self.resolution;
            } else {
                isrc.x -= self.resolution;
            }

            if i & 2 != 0 {
                isrc.z += self.resolution;
            } else {
                isrc.z -= self.resolution;
            }

            if i & 4 != 0 {
                idst.x += self.resolution;
            } else {
                idst.x -= self.resolution;
            }

            if i & 8 != 0 {
                idst.z += self.resolution;
            } else {
                idst.z -= self.resolution;
            }

            if chunk.owns(&isrc) && chunk.owns(&idst) {
                return true;
            }

            // raise y a bit to avoid collision error in some conditions
            isrc.y += physics.get_scramble_height();
            idst.y += physics.get_scramble_height();

            if chunk.owns(&isrc) && chunk.owns(&idst) {
                return true;
            }
        }
        false
    }

    /// `fill` (`waypoint_flood.cpp:306-726`): flood from a seed point.
    /// Returns the number of points processed, 0 on failure. The
    /// reference's `BgTaskManager` abort paths and the
    /// `DEBUG_WAYPOINT_FLOOD`-only `LogFile`/fixed-point debug lines
    /// (`waypoint_flood.cpp:311-315`) are omitted — the port is a
    /// synchronous single-threaded fill (registered).
    pub fn fill(
        &mut self,
        seed_point: &Vector3,
        physics: &mut dyn FloodPhysics,
        chunk: &dyn FloodChunk,
        mut progress: Option<&mut dyn FloodProgress>,
    ) -> i32 {
        let mut count = 0i32;
        let mut flood_stack: Vec<(Vector3, i32)> = Vec::new();

        let mut src = Vector3::ZERO;
        src.x = ((seed_point.x - self.min.x) / self.resolution) as i32 as f32 * self.resolution
            + self.min.x;
        src.z = ((seed_point.z - self.min.z) / self.resolution) as i32 as f32 * self.resolution
            + self.min.z;
        src.y = seed_point.y + 0.1;

        // drop the source point
        let mut drop_point = 0.0;
        if !physics.find_drop_point(&src, &mut drop_point) {
            // the reference warning is gated on pProgress != NULL
            if progress.is_some() {
                // WARNING_MSG dropped (registered: silent)
            }
            return 0;
        }

        src.y = drop_point;

        if !self.check_bounds(&src, &src, physics, chunk) {
            src.x += if seed_point.x >= 0.0 {
                self.resolution
            } else {
                -self.resolution
            };
            src.y = seed_point.y + 0.1;
            let mut ok = physics.find_drop_point(&src, &mut drop_point);
            if ok {
                src.y = drop_point;
                ok = self.check_bounds(&src, &src, physics, chunk);
            }
            if !ok {
                src.x -= if seed_point.x >= 0.0 {
                    self.resolution
                } else {
                    -self.resolution
                };
                src.z += if seed_point.z >= 0.0 {
                    self.resolution
                } else {
                    -self.resolution
                };
                src.y = seed_point.y + 0.1;
                let mut ok = physics.find_drop_point(&src, &mut drop_point);
                if ok {
                    src.y = drop_point;
                    ok = self.check_bounds(&src, &src, physics, chunk);
                }
                if !ok {
                    src.x += if seed_point.x >= 0.0 {
                        self.resolution
                    } else {
                        -self.resolution
                    };
                    src.y = seed_point.y + 0.1;
                    let mut ok = physics.find_drop_point(&src, &mut drop_point);
                    if ok {
                        src.y = drop_point;
                        ok = self.check_bounds(&src, &src, physics, chunk);
                    }
                    if !ok {
                        return 0;
                    }
                }
            }
        }

        // Make sure our initial point is in some grid
        let src_x = ((src.x - self.min.x) / self.resolution + 0.5) as i32;
        let src_z = ((src.z - self.min.z) / self.resolution + 0.5) as i32;
        if src_x < 0 || src_x >= self.xsize || src_z < 0 || src_z >= self.zsize {
            // ERROR_MSG dropped (registered: silent), same return 0
            return 0;
        }

        let src_index = src_x + src_z * self.xsize;

        // find the layer at the seed cell that matches the dropped height
        let mut best_g = 0i32;
        let mut min_g_dist = 100000.0f32;
        for g in (1..MAX_HEIGHTS as i32).rev() {
            if (self.hgt_grids[g as usize][src_index as usize] - src.y).abs() < HEIGHT_THRESHOLD
                && physics.is_unblocked(&src, self.hgt_grids[g as usize][src_index as usize])
            {
                let dist = (self.hgt_grids[g as usize][src_index as usize] - src.y).abs();
                if dist < min_g_dist {
                    min_g_dist = dist;
                    best_g = g;
                }
            }
        }
        let mut g = best_g;

        // Add it to an available grid if it is not
        if g == 0 {
            for cand in (1..MAX_HEIGHTS as i32).rev() {
                if self.hgt_grids[cand as usize][src_index as usize] == INITIAL_HEIGHT {
                    g = cand;
                    break;
                }
            }

            if g == 0 {
                // ERROR_MSG dropped (registered: silent), same return 0
                return 0;
            }
            self.hgt_grids[g as usize][src_index as usize] = src.y;

            if g < self.smallest_height {
                self.smallest_height = g;
            }
        }
        let src_grid = g;

        if self.adj_grids[src_grid as usize][src_index as usize].all == 0 {
            flood_stack.push((src, src_grid));
        } else {
            // already filled
            return 0;
        }

        let save_g = src_grid;
        let save_index = src_index;
        let mut got_one = false;

        // Now seed and expand the stack
        while let Some((src, src_grid)) = flood_stack.pop() {
            count += 1;
            if count % 200 == 0
                && progress
                    .as_deref_mut()
                    .is_some_and(|progress| progress.filled(count))
            {
                return -1; // request stop processing
            }

            let src_x = ((src.x - self.min.x) / self.resolution + 0.5) as i32;
            let src_z = ((src.z - self.min.z) / self.resolution + 0.5) as i32;
            let src_index = src_x + src_z * self.xsize;

            for angle in 0..8usize {
                if self.adj_grids[src_grid as usize][src_index as usize].angle(angle) != 0 {
                    continue;
                }

                let dst_x = src_x + DX[angle];
                let dst_z = src_z + DZ[angle];
                let dst_index = dst_x + dst_z * self.xsize;

                let mut dst = Vector3::new(
                    self.min.x + dst_x as f32 * self.resolution,
                    src.y,
                    self.min.z + dst_z as f32 * self.resolution,
                );

                // not on grid.
                if dst_x < 0 || dst_x >= self.xsize || dst_z < 0 || dst_z >= self.zsize {
                    continue;
                }

                // not in space
                if dst.x < chunk.min_coord_x()
                    || dst.x > chunk.max_coord_x()
                    || dst.z < chunk.min_coord_z()
                    || dst.z > chunk.max_coord_z()
                {
                    continue;
                }

                // Check that movement is possible from src to dst
                let mut dst2 = Vector3::ZERO;
                physics.adjust_move(&src, &dst, &mut dst2);
                if (dst.x - dst2.x).abs() > FLOOD_FLOAT_FUDGE
                    || (dst.z - dst2.z).abs() > FLOOD_FLOAT_FUDGE
                    || (dst.y - dst2.y).abs() > HEIGHT_THRESHOLD * 10.0
                {
                    continue;
                }

                // Now we know the correct y value for the destination,
                dst.y = dst2.y;
                // so we can determine which grid it should occupy.

                let mut best_g = 0i32;
                let mut min_g_dist = 100000.0f32;
                for g in (1..MAX_HEIGHTS as i32).rev() {
                    if (self.hgt_grids[g as usize][dst_index as usize] - dst.y).abs()
                        < HEIGHT_THRESHOLD
                        && physics
                            .is_unblocked(&dst, self.hgt_grids[g as usize][dst_index as usize])
                    {
                        // the reference computes this distance against the
                        // SOURCE cell's height, not the destination's
                        // (`waypoint_flood.cpp:578`) — bug kept faithfully
                        let dist = (self.hgt_grids[g as usize][src_index as usize] - src.y).abs();
                        if dist < min_g_dist {
                            min_g_dist = dist;
                            best_g = g;
                        }
                    }
                }
                let mut g = best_g;

                let mut dst_grid = g;
                if g == 0 {
                    g = 0;
                    for cand in (1..MAX_HEIGHTS as i32).rev() {
                        if self.hgt_grids[cand as usize][dst_index as usize] == INITIAL_HEIGHT {
                            g = cand;
                            break;
                        }
                    }
                    if g == 0 {
                        // ERROR_MSG dropped (registered: silent)
                        continue;
                    }
                    // we set the height up later
                    if self.adj_grids[g as usize][dst_index as usize].all != 0 {
                        // CRITICAL_MSG: the "free height but live adjacency"
                        // invariant violation kills the reference process
                        panic!(
                            "in grid {} index {} ({},{}), height is {} but adj is {}!",
                            g,
                            dst_index,
                            dst_x,
                            dst_z,
                            self.hgt_grids[g as usize][dst_index as usize],
                            self.adj_grids[g as usize][dst_index as usize].all
                        );
                    }
                    dst_grid = g;
                }

                // Now try moving back again, from dst to src
                let mut src2 = Vector3::ZERO;
                physics.adjust_move(&dst2, &src, &mut src2);

                if ((src.x - src2.x).abs() > FLOOD_FLOAT_FUDGE
                    || (src.z - src2.z).abs() > FLOOD_FLOAT_FUDGE)
                    || (src.y - src2.y).abs() > 0.1
                {
                    continue;
                }

                if self.adj_grids[dst_grid as usize][dst_index as usize].angle(angle ^ 4) != 0
                    && self.adj_grids[dst_grid as usize][dst_index as usize].angle(angle ^ 4)
                        != src_grid as u32
                {
                    g = 0;
                    for cand in (1..MAX_HEIGHTS as i32).rev() {
                        if self.hgt_grids[cand as usize][dst_index as usize] == INITIAL_HEIGHT {
                            g = cand;
                            break;
                        }
                    }
                    if g == 0 {
                        // ERROR_MSG dropped (registered: silent)
                        continue;
                    }
                    // we set the height up later
                    if self.adj_grids[g as usize][dst_index as usize].all != 0 {
                        // CRITICAL_MSG arm of the second allocation block
                        // (`waypoint_flood.cpp:655-662`); kills the
                        // reference process
                        panic!(
                            "in grid {} index {} ({},{}), height is {} but adj is {}!",
                            g,
                            dst_index,
                            dst_x,
                            dst_z,
                            self.hgt_grids[g as usize][dst_index as usize],
                            self.adj_grids[g as usize][dst_index as usize].all
                        );
                    }
                    dst_grid = g;

                    // Now try moving back again, from dst to src
                    let mut src2 = Vector3::ZERO;
                    physics.adjust_move(&dst2, &src, &mut src2);
                    if ((src.x - src2.x).abs() > FLOOD_FLOAT_FUDGE
                        || (src.z - src2.z).abs() > FLOOD_FLOAT_FUDGE)
                        || (src.y - src2.y).abs() > 0.1
                    {
                        // "Shall NEVER fail here!"
                        continue;
                    }
                }

                // If the destination has no adjacencies yet, add it to
                // the stack, so we'll process it later.
                if self.adj_grids[dst_grid as usize][dst_index as usize].all == 0 {
                    // Unless it's out of bounds, in which case do not add it at all
                    if !self.check_bounds(&src, &dst, physics, chunk) {
                        self.boundary_set.insert((src_index, src_grid));
                        continue;
                    }
                    self.hgt_grids[dst_grid as usize][dst_index as usize] = dst.y;

                    if dst_grid < self.smallest_height {
                        self.smallest_height = dst_grid;
                    }

                    got_one = true;
                    flood_stack.push((dst, dst_grid));
                }

                // Set the adjacency information
                if self.adj_grids[src_grid as usize][src_index as usize].angle(angle) == 0
                    && self.adj_grids[dst_grid as usize][dst_index as usize].angle(angle ^ 4) == 0
                {
                    self.adj_grids[src_grid as usize][src_index as usize]
                        .set_angle(angle, dst_grid as u32);
                    self.adj_grids[dst_grid as usize][dst_index as usize]
                        .set_angle(angle ^ 4, src_grid as u32);
                    got_one = true;
                }
            }
        }

        if !got_one {
            self.hgt_grids[save_g as usize][save_index as usize] = INITIAL_HEIGHT;
        }

        count
    }

    /// `flashFlood` (`waypoint_flood.cpp:732-791`): fill the whole area
    /// at the top layer with the given height, fully interconnected.
    pub fn flash_flood(&mut self, height: f32) {
        let hgt_grid = MAX_HEIGHTS - 1;
        let xsize = self.xsize;
        let zsize = self.zsize;

        // fill it all in
        for x in 0..xsize {
            for z in 0..zsize {
                let index = x + z * xsize;
                self.adj_grids[hgt_grid][index as usize].all = 0xffffffff;
                self.hgt_grids[hgt_grid][index as usize] = height;
            }
        }

        // fix up bottom and top
        for x in 0..xsize {
            let aindex = x;
            let bindex = x + (zsize - 1) * xsize;

            self.adj_grids[hgt_grid][aindex as usize].set_angle(3, 0);
            self.adj_grids[hgt_grid][bindex as usize].set_angle(7, 0);
            self.adj_grids[hgt_grid][aindex as usize].set_angle(4, 0);
            self.adj_grids[hgt_grid][bindex as usize].set_angle(0, 0);
            self.adj_grids[hgt_grid][aindex as usize].set_angle(5, 0);
            self.adj_grids[hgt_grid][bindex as usize].set_angle(1, 0);
        }

        // fix up left and right
        for z in 0..zsize {
            let aindex = z * xsize;
            let bindex = xsize - 1 + z * xsize;

            self.adj_grids[hgt_grid][aindex as usize].set_angle(5, 0);
            self.adj_grids[hgt_grid][bindex as usize].set_angle(1, 0);
            self.adj_grids[hgt_grid][aindex as usize].set_angle(6, 0);
            self.adj_grids[hgt_grid][bindex as usize].set_angle(2, 0);
            self.adj_grids[hgt_grid][aindex as usize].set_angle(7, 0);
            self.adj_grids[hgt_grid][bindex as usize].set_angle(3, 0);
        }

        let dy: [i32; 8] = [xsize, xsize, 0, -xsize, -xsize, -xsize, 0, xsize];

        // now remove the corner points completely
        for a in (0..8).step_by(2) {
            let index = (if a & 4 != 0 { xsize - 1 } else { 0 })
                + (if (a as u32).wrapping_sub(2) < 4 {
                    zsize - 1
                } else {
                    0
                }) * xsize;
            self.adj_grids[hgt_grid][index as usize].all = 0;
            for k in 0..3 {
                let n = (a + k) & 7;
                self.adj_grids[hgt_grid][(index + DX[n] + dy[n]) as usize]
                    .set_angle((n + 4) & 7, 0);
            }
        }
    }

    /// `postfilteradd` (`waypoint_flood.cpp:797-953`): smooth isolated
    /// missing links and fill in the diagonal of a fully linked square.
    pub fn postfilter_add(&mut self) {
        let mut orig_grids: Vec<Vec<AdjGridElt>> = (0..MAX_HEIGHTS)
            .map(|g| self.adj_grids[g].clone())
            .collect();

        let dy: [i32; 8] = [
            self.xsize,
            self.xsize,
            0,
            -self.xsize,
            -self.xsize,
            -self.xsize,
            0,
            self.xsize,
        ];

        // Smooth out islands of points that are unpassable from certain
        // directions but passable from others.
        let mut gotone;
        loop {
            gotone = false;
            for g in self.smallest_height..MAX_HEIGHTS as i32 {
                for x in 0..self.xsize {
                    for z in 0..self.zsize {
                        let index = x + z * self.xsize;

                        for i in (0..8).step_by(2) {
                            if self.adj_grids[g as usize][index as usize].angle(i) != 0 {
                                continue;
                            }

                            if i == 0 && z == self.zsize - 1 {
                                continue;
                            }
                            if i == 2 && x == self.xsize - 1 {
                                continue;
                            }
                            if i == 4 && z == 0 {
                                continue;
                            }
                            if i == 6 && x == 0 {
                                continue;
                            }

                            let ondex = index + DX[i] + dy[i];
                            let mut og = 0u32;

                            let mut tindex = index + DX[(i + 2) & 7] + dy[(i + 2) & 7];
                            let mut tg = orig_grids[g as usize][index as usize].angle((i + 2) & 7);
                            if og == 0 && tg != 0 {
                                og = orig_grids[tg as usize][tindex as usize].angle((i + 7) & 7);
                            }
                            tindex = index + DX[(i + 6) & 7] + dy[(i + 6) & 7];
                            tg = orig_grids[g as usize][index as usize].angle((i + 6) & 7);
                            if og == 0 && tg != 0 {
                                og = orig_grids[tg as usize][tindex as usize].angle((i + 1) & 7);
                            }

                            if og != 0 {
                                self.adj_grids[g as usize][index as usize].set_angle(i, og);
                                self.adj_grids[og as usize][ondex as usize]
                                    .set_angle(i ^ 4, g as u32);
                                gotone = true;
                            }
                        }
                    }
                }
            }

            for g in self.smallest_height..MAX_HEIGHTS as i32 {
                orig_grids[g as usize] = self.adj_grids[g as usize].clone();
            }

            if !gotone {
                break;
            }
        }

        for g in self.smallest_height..MAX_HEIGHTS as i32 {
            for x in 1..self.xsize {
                for z in 1..self.zsize {
                    let index = x + z * self.xsize;

                    // fill the diagonal of a square linked on all four
                    // straight sides through the same layer chain
                    let lg = orig_grids[g as usize][index as usize].angle(6);
                    let dg = orig_grids[lg as usize][(index - 1) as usize].angle(4);
                    let rg = orig_grids[dg as usize][(index - self.xsize - 1) as usize].angle(2);
                    let ug = orig_grids[rg as usize][(index - self.xsize) as usize].angle(0);
                    // the reference chains these probes with
                    // short-circuit &&; the eager reads land on live
                    // grids either way and the guard still
                    // short-circuits on the zero checks
                    if lg != 0 && dg != 0 && rg != 0 && ug != 0 && ug == g as u32 {
                        self.adj_grids[g as usize][index as usize].set_angle(5, dg);
                        self.adj_grids[lg as usize][(index - 1) as usize].set_angle(3, rg);
                        self.adj_grids[dg as usize][(index - self.xsize - 1) as usize]
                            .set_angle(1, g as u32);
                        self.adj_grids[rg as usize][(index - self.xsize) as usize].set_angle(7, lg);
                    }
                }
            }
        }

        self.check_grid_consistency();
    }

    /// `postfilterremove` (`waypoint_flood.cpp:955-1097`): remove
    /// concave corner points that would break the polygon walk.
    pub fn postfilter_remove(&mut self) {
        let orig_grids: Vec<Vec<AdjGridElt>> = (0..MAX_HEIGHTS)
            .map(|g| self.adj_grids[g].clone())
            .collect();

        let xdirs: [i32; 4] = [1, -self.xsize, -1, self.xsize];
        let zdirs: [i32; 4] = [self.xsize, 1, -self.xsize, -1];
        let anglemap: [i32; 9] = [5, 4, 3, 6, -1, 2, 7, 0, 1];

        for g in self.smallest_height..MAX_HEIGHTS as i32 {
            for x in 1..self.xsize - 1 {
                for z in 1..self.zsize - 1 {
                    let index = x + z * self.xsize;

                    if self.boundary_set.contains(&(index, g)) {
                        continue;
                    }

                    for a in (0..8).step_by(2) {
                        // relative angle (a + k) & 7
                        let av = |k: usize| (a + k) & 7;

                        let mut ondex = [0i32; 9];
                        let mut og = [0u32; 9];
                        for i in 0..9usize {
                            ondex[i] = index
                                + xdirs[a >> 1] * ((i as i32 % 3) - 1)
                                + zdirs[a >> 1] * ((i as i32 / 3) - 1);
                            og[i] = if anglemap[i] != -1 {
                                self.adj_grids[g as usize][index as usize]
                                    .angle(av(anglemap[i] as usize))
                            } else {
                                g as u32
                            };
                        }

                        // angle of an offset cell's orig grid entry
                        let oga = |oi: usize, k: usize, gi: u32| {
                            orig_grids[gi as usize][ondex[oi] as usize].angle(av(k))
                        };
                        let center = &orig_grids[g as usize][index as usize];

                        // the concave-corner test, all twenty clauses
                        let cond = og[3] != 0
                            && oga(3, 1, og[3]) == 0
                            && oga(3, 2, og[3]) != 0
                            && oga(3, 3, og[3]) != 0
                            && oga(3, 4, og[3]) == og[0]
                            && og[1] != 0
                            && oga(1, 1, og[1]) == 0
                            && oga(1, 0, og[1]) != 0
                            && oga(1, 7, og[1]) != 0
                            && oga(1, 6, og[1]) == og[0]
                            && center.angle(av(6)) != 0
                            && center.angle(av(5)) != 0
                            && center.angle(av(4)) != 0
                            && center.angle(av(3)) == 0
                            && center.angle(av(1)) == 0
                            && center.angle(av(7)) == 0
                            && (og[3] != 0 && oga(3, 0, og[3]) != 0
                                || og[1] != 0 && oga(1, 2, og[1]) != 0);

                        if cond {
                            let g1 = og[1] as usize;
                            let g3 = og[3] as usize;
                            self.adj_grids[g1][ondex[1] as usize].set_angle(av(0), 0);
                            self.adj_grids[g1][ondex[1] as usize].set_angle(av(1), 0);
                            self.adj_grids[og[0] as usize][ondex[0] as usize].set_angle(av(1), 0);
                            self.adj_grids[g3][ondex[3] as usize].set_angle(av(1), 0);
                            self.adj_grids[g3][ondex[3] as usize].set_angle(av(2), 0);

                            self.adj_grids[g as usize][index as usize].all = 0;

                            if og[5] != 0 {
                                self.adj_grids[og[5] as usize][ondex[5] as usize]
                                    .set_angle(av(6), 0);
                            }
                            if og[7] != 0 {
                                self.adj_grids[og[7] as usize][ondex[7] as usize]
                                    .set_angle(av(4), 0);
                            }
                        }
                    }
                }
            }
        }

        self.check_grid_consistency();
    }

    /// `checkGridConsistency` (`waypoint_flood.cpp:1102-1136`): every
    /// link must be reciprocated by the layer it names.
    pub fn check_grid_consistency(&self) -> bool {
        let dy: [i32; 8] = [
            self.xsize,
            self.xsize,
            0,
            -self.xsize,
            -self.xsize,
            -self.xsize,
            0,
            self.xsize,
        ];

        for g in 0..MAX_HEIGHTS {
            for x in 1..self.xsize - 1 {
                for z in 1..self.zsize - 1 {
                    let sindex = x + z * self.xsize;
                    for angle in 0..8usize {
                        let dindex = sindex + DX[angle] + dy[angle];

                        let dg = self.adj_grids[g][sindex as usize].angle(angle);
                        if dg != 0
                            && self.adj_grids[dg as usize][dindex as usize].angle(angle ^ 4)
                                != g as u32
                        {
                            // ERROR_MSG dropped (registered: silent)
                            return false;
                        }
                    }
                }
            }
        }
        true
    }

    /// `shrink` (`waypoint_flood.cpp:1142-1202`): a no-op. The entire
    /// body of the reference method is either allocation bookkeeping or
    /// `#if 0`-disabled logic (`waypoint_flood.cpp:1156-1197`).
    pub fn shrink(&self) {}

    /// `writeTGA` (`waypoint_flood.cpp:1214-1257`): dump the adjacency
    /// and height layers as a palette TGA whose pixels are the raw grid
    /// bytes, reshaped to `xsize*MAX_HEIGHTS*4` by `zsize*MAX_HEIGHTS*4`.
    /// The 13-byte extra-data field carries `min` and `resolution` so the
    /// vertices can be recomputed later.
    pub fn write_tga(&self, filename: &str) -> bool {
        let width = (self.xsize * MAX_HEIGHTS as i32 * 4) as u16;
        let height = (self.zsize * MAX_HEIGHTS as i32 * 4) as u16;
        let mut data =
            Vec::with_capacity(18 + 12 + 4 + 768 + 2 * MAX_HEIGHTS * self.size as usize * 4);
        // [13, 1, 1, 0, 0, 0, 1, 24, 0, 0, 0, 0, w_lo, w_hi, h_lo, h_hi, 8, 0]
        data.extend_from_slice(&[
            (4 * 4) as u8,
            1,
            1,
            0,
            0,
            0,
            1,
            24,
            0,
            0,
            0,
            0,
            (width & 0xff) as u8,
            (width >> 8) as u8,
            (height & 0xff) as u8,
            (height >> 8) as u8,
            8,
            0,
        ]);
        for v in [self.min.x, self.min.y, self.min.z, self.resolution] {
            data.extend_from_slice(&v.to_le_bytes());
        }
        for i in 0..256u32 {
            let b = i as u8;
            data.extend_from_slice(&[b, b, b]);
        }
        for grid in &self.adj_grids {
            for elt in grid {
                data.extend_from_slice(&elt.all.to_le_bytes());
            }
        }
        for grid in &self.hgt_grids {
            for h in grid {
                data.extend_from_slice(&h.to_le_bytes());
            }
        }
        std::fs::write(filename, data).is_ok()
    }
}

#[cfg(test)]
pub(crate) mod artifacts;
#[cfg(test)]
mod covers;
#[cfg(test)]
pub(crate) mod tests;
