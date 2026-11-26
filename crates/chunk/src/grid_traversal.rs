//! BW-10-07-04: the space grid walk (`lib/chunk/grid_traversal.hpp`) —
//! the line a collide sweep draws through the chunk grid, including the
//! "near" side cells a wide shape overlaps and the LIFO alternate cells
//! they queue up.
//!
//! Every visited cell answers the same four facts the collide loop
//! consumes: `sx`/`sz` and the travel window `cellSTravel`/`cellETravel`
//! (correct at all times for the current cell, as the reference header
//! puts it). The construction pops one alternate cell before the first
//! answer, so the first visited cell is not always the source's cell —
//! the dumps pinned in the tests below all start from that state.

use foundation::math::vector::Vector3;

/// `SpaceGridTraversal::CellSpec` — a queued side cell.
#[derive(Debug, Clone, Copy)]
struct CellSpec {
    x: i32,
    z: i32,
    s_travel: f32,
    e_travel: f32,
}

/// `SpaceGridTraversal` (`grid_traversal.hpp:16-387`).
pub struct SpaceGridTraversal {
    /// `extent` — the sweep's far point.
    pub extent: Vector3,
    /// `dir` — the normalised sweep direction.
    pub dir: Vector3,

    /// `gridResF_`.
    grid_res_f: f32,

    /// `fullDist`.
    pub full_dist: f32,
    /// `allOver`.
    all_over: bool,

    /// `xsense` / `zsense` — the direction's axis signs (-1/0/1).
    pub xsense: i32,
    pub zsense: i32,

    /// `sx` / `sz` — the current cell.
    pub sx: i32,
    pub sz: i32,
    /// `ex` / `ez` — the extent's cell.
    pub ex: i32,
    pub ez: i32,

    /// `cellSTravel` / `cellETravel` — the current cell's travel window.
    pub cell_s_travel: f32,
    pub cell_e_travel: f32,

    /// `cast` — the last landing point carried into the next step.
    cast: Vector3,
    /// `land`.
    land: Vector3,

    /// `cellETravelPrivate` — `cellETravel` across `calcNext` calls.
    cell_e_travel_private: f32,

    /// `xrange` / `zrange` — the swept shape's extents.
    x_range: f32,
    z_range: f32,
    /// `xseep` / `zseep` — `gridResF_ - range`.
    xseep: f32,
    zseep: f32,

    /// `nx` / `nz` — the next main-line cell.
    nx: i32,
    nz: i32,

    /// `gx` / `gz` — the near set's grid offset (`offNear`).
    gx: i32,
    gz: i32,

    /// `bx` / `bz` — the offset's parity (`offNear`).
    bx: i32,
    bz: i32,

    /// `onRight` / `onAbove`.
    on_right: bool,
    on_above: bool,

    /// `nearCount[2][2]` — the reference's `char` counters (counts reach
    /// 4 at most; `i32` is a registered widening).
    near_count: [[i32; 2]; 2],
    /// `nearSTravel[2][2]`.
    near_s_travel: [[f32; 2]; 2],

    /// `altCells` — the reference bounds this to 1024 entries via
    /// `StaticArray`; the port grows unbounded (registered).
    alt_cells: Vec<CellSpec>,
}

impl SpaceGridTraversal {
    /// `SpaceGridTraversal(source, extent_i, range, gridResF)`
    /// (`grid_traversal.hpp:19-74`).
    pub fn new(source: Vector3, extent: Vector3, range: Vector3, grid_res_f: f32) -> Self {
        let mut t = Self {
            extent,
            dir: Vector3::ZERO,
            grid_res_f,
            full_dist: 0.0,
            all_over: false,
            xsense: 0,
            zsense: 0,
            sx: 0,
            sz: 0,
            ex: 0,
            ez: 0,
            cell_s_travel: 0.0,
            cell_e_travel: 0.0,
            cast: Vector3::ZERO,
            land: Vector3::ZERO,
            cell_e_travel_private: 0.0,
            x_range: 0.0,
            z_range: 0.0,
            xseep: 0.0,
            zseep: 0.0,
            nx: 0,
            nz: 0,
            gx: 0,
            gz: 0,
            bx: 0,
            bz: 0,
            on_right: false,
            on_above: false,
            near_count: [[0; 2]; 2],
            near_s_travel: [[0.0; 2]; 2],
            alt_cells: Vec::new(),
        };

        // first find what column 'source' and 'extent' are in
        t.sx = (source.x / grid_res_f) as i32;
        if source.x < 0.0 {
            t.sx -= 1;
        }
        t.sz = (source.z / grid_res_f) as i32;
        if source.z < 0.0 {
            t.sz -= 1;
        }

        t.ex = (extent.x / grid_res_f) as i32;
        if extent.x < 0.0 {
            t.ex -= 1;
        }
        t.ez = (extent.z / grid_res_f) as i32;
        if extent.z < 0.0 {
            t.ez -= 1;
        }

        // find out which way we're going
        t.dir = extent - source;
        t.xsense = if t.dir.x < 0.0 {
            -1
        } else if t.dir.x > 0.0 {
            1
        } else {
            0
        };
        t.zsense = if t.dir.z < 0.0 {
            -1
        } else if t.dir.z > 0.0 {
            1
        } else {
            0
        };

        // record how big the the thing we're moving through is
        t.x_range = range.x;
        t.z_range = range.z;
        t.xseep = grid_res_f - range.x;
        t.zseep = grid_res_f - range.z;

        // find length of line
        t.full_dist = t.dir.length();
        t.dir = t.dir / t.full_dist;

        // prepare for first call of next
        t.land = source;
        t.cell_e_travel_private = 0.0;
        t.nx = t.sx;
        t.nz = t.sz;
        t.all_over = false;

        t.on_right = t.land.x - t.nx as f32 * grid_res_f > t.xseep;
        t.on_above = t.land.z - t.nz as f32 * grid_res_f > t.zseep;
        t.near_s_travel[0][0] = 0.0;
        t.near_s_travel[0][1] = 0.0;
        t.near_s_travel[1][0] = 0.0;
        t.near_s_travel[1][1] = 0.0;
        t.off_near(t.sx, t.sz);
        t.near_count[0][0] = 0;
        t.near_count[0][1] = 0;
        t.near_count[1][0] = 0;
        t.near_count[1][1] = 0;
        t.inc_near(0, 0, 0.0);
        t.inc_near(0, t.on_above as i32, 0.0);
        t.inc_near(t.on_right as i32, 0, 0.0);
        t.inc_near(t.on_right as i32, t.on_above as i32, 0.0);

        // calculate cast and land for this square, and nx and nz.
        t.calc_next();

        // MF_VERIFY(popAltCell())
        debug_assert!(t.pop_alt_cell());

        t
    }

    /// `next` (`grid_traversal.hpp:76-107`) — the reference name is kept
    /// (not the `Iterator` shape): the walk consumes cells through an
    /// explicit `while`, exactly like the collide loop does.
    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> bool {
        if self.pop_alt_cell() {
            return true;
        }

        if self.all_over {
            return false;
        }

        self.sx = self.nx;
        self.sz = self.nz;

        self.calc_next();

        // MF_VERIFY(popAltCell())
        debug_assert!(self.pop_alt_cell());

        true
    }

    /// `allOver`.
    pub fn all_over(&self) -> bool {
        self.all_over
    }

    /// `incNear` (`grid_traversal.hpp:165-174`): `!nearCount[r][a]++`
    /// snapshots the zero check before the increment.
    fn inc_near(&mut self, r: i32, a: i32, s_trav: f32) {
        let ri = (r ^ self.bx) as usize;
        let ai = (a ^ self.bz) as usize;

        if self.near_count[ri][ai] == 0 {
            self.near_s_travel[ri][ai] = s_trav;
        }
        self.near_count[ri][ai] += 1;
    }

    /// `offNear` (`grid_traversal.hpp:176-182`).
    fn off_near(&mut self, x: i32, z: i32) {
        self.gx = x;
        self.gz = z;
        self.bx = x & 1;
        self.bz = z & 1;
    }

    /// `decNear` (`grid_traversal.hpp:184-193`): the last release of a
    /// corner queues its cell. The pushed coordinates un-xor the indices
    /// (`gx + (r ^ bx)` after `r ^= bx`), which is the original `r`.
    fn dec_near(&mut self, r: i32, a: i32, e_trav: f32) {
        let ri = (r ^ self.bx) as usize;
        let ai = (a ^ self.bz) as usize;
        self.near_count[ri][ai] -= 1;
        if self.near_count[ri][ai] == 0 {
            self.alt_cells.push(CellSpec {
                x: self.gx + r,
                z: self.gz + a,
                s_travel: self.near_s_travel[ri][ai],
                e_travel: e_trav,
            });
        }
    }

    /// `calcNext` (`grid_traversal.hpp:195-371`).
    fn calc_next(&mut self) {
        self.cast = self.land;
        self.land = self.extent;

        self.cell_e_travel = self.cell_e_travel_private;
        self.cell_s_travel = self.cell_e_travel;

        // before returning, set up nearCount for nx, nz, and add any
        // alternate cells that need to be looked at before next call

        // find next point on the face of a column
        let mut try_dist;
        let mut try_other;
        if self.xsense != 0 {
            let x_edge = (if self.xsense < 0 {
                self.sx
            } else {
                self.sx + 1
            }) as f32
                * self.grid_res_f;
            try_dist = (x_edge - self.cast.x) / self.dir.x;
            try_other = self.cast.z + try_dist * self.dir.z;
            if try_other >= self.sz as f32 * self.grid_res_f
                && try_other <= (self.sz + 1) as f32 * self.grid_res_f
                && self.cell_e_travel + try_dist < self.full_dist
            {
                self.land.x = x_edge;
                self.land.y = self.cast.y + try_dist * self.dir.y;
                self.land.z = try_other;
                self.nx = self.sx + self.xsense;
                self.cell_e_travel += try_dist;

                // update our cell counters
                if self.xsense < 0 {
                    // moving left

                    // take out right points
                    self.dec_near(self.on_right as i32, 0, self.cell_e_travel);
                    self.dec_near(
                        self.on_right as i32,
                        self.on_above as i32,
                        self.cell_e_travel,
                    );

                    // offset grid
                    self.off_near(self.nx, self.nz);
                    self.on_right = self.x_range > 0.0; // since pt on extreme right

                    // move left points
                    self.inc_near(0, 0, self.cell_e_travel);
                    self.inc_near(0, self.on_above as i32, self.cell_e_travel);
                    self.dec_near(1, 0, self.cell_e_travel);
                    self.dec_near(1, self.on_above as i32, self.cell_e_travel);

                    // put back right points
                    self.inc_near(self.on_right as i32, 0, self.cell_e_travel);
                    self.inc_near(
                        self.on_right as i32,
                        self.on_above as i32,
                        self.cell_e_travel,
                    );
                } else {
                    // moving right

                    // move left points
                    self.inc_near(1, 0, self.cell_e_travel);
                    self.inc_near(1, self.on_above as i32, self.cell_e_travel);
                    self.dec_near(0, 0, self.cell_e_travel);
                    self.dec_near(0, self.on_above as i32, self.cell_e_travel);

                    // take out right points
                    self.dec_near(self.on_right as i32, 0, self.cell_e_travel);
                    self.dec_near(
                        self.on_right as i32,
                        self.on_above as i32,
                        self.cell_e_travel,
                    );

                    // offset grid
                    self.off_near(self.nx, self.nz);
                    self.on_right = false; // since pt on extreme left

                    // put back right points
                    self.inc_near(self.on_right as i32, 0, self.cell_e_travel);
                    self.inc_near(
                        self.on_right as i32,
                        self.on_above as i32,
                        self.cell_e_travel,
                    );
                }
            }
        }
        if self.zsense != 0 {
            let z_edge = (if self.zsense < 0 {
                self.sz
            } else {
                self.sz + 1
            }) as f32
                * self.grid_res_f;
            try_dist = (z_edge - self.cast.z) / self.dir.z;
            try_other = self.cast.x + try_dist * self.dir.x;
            if try_other >= self.sx as f32 * self.grid_res_f
                && try_other <= (self.sx + 1) as f32 * self.grid_res_f
                && self.cell_e_travel + try_dist < self.full_dist
            {
                self.land.x = try_other;
                self.land.y = self.cast.y + try_dist * self.dir.y;
                self.land.z = z_edge;
                self.nz = self.sz + self.zsense;
                if self.nx == self.sx {
                    self.cell_e_travel += try_dist;
                }

                // update our cell counters
                if self.zsense < 0 {
                    // moving down

                    // take out above points
                    self.dec_near(0, self.on_above as i32, self.cell_e_travel);
                    self.dec_near(
                        self.on_right as i32,
                        self.on_above as i32,
                        self.cell_e_travel,
                    );

                    // offset grid
                    self.off_near(self.nx, self.nz);
                    self.on_above = self.z_range > 0.0; // since pt on extreme top

                    // move below points
                    self.inc_near(0, 0, self.cell_e_travel);
                    self.inc_near(self.on_right as i32, 0, self.cell_e_travel);
                    self.dec_near(0, 1, self.cell_e_travel);
                    self.dec_near(self.on_right as i32, 1, self.cell_e_travel);

                    // put back above points
                    self.inc_near(0, self.on_above as i32, self.cell_e_travel);
                    self.inc_near(
                        self.on_right as i32,
                        self.on_above as i32,
                        self.cell_e_travel,
                    );
                } else {
                    // moving up

                    // move below points
                    self.inc_near(0, 1, self.cell_e_travel);
                    self.inc_near(self.on_right as i32, 1, self.cell_e_travel);
                    self.dec_near(0, 0, self.cell_e_travel);
                    self.dec_near(self.on_right as i32, 0, self.cell_e_travel);

                    // take out above points
                    self.dec_near(0, self.on_above as i32, self.cell_e_travel);
                    self.dec_near(
                        self.on_right as i32,
                        self.on_above as i32,
                        self.cell_e_travel,
                    );

                    // offset grid
                    self.off_near(self.nx, self.nz);
                    self.on_above = false; // since pt on extreme bottom

                    // put back above points
                    self.inc_near(0, self.on_above as i32, self.cell_e_travel);
                    self.inc_near(
                        self.on_right as i32,
                        self.on_above as i32,
                        self.cell_e_travel,
                    );
                }
            }
        }

        // if we didn't move to another x cell, see if the right
        // corners are in a different region (i.e. has onRight changed)
        if self.sx == self.nx {
            let nn_right = self.land.x - self.nx as f32 * self.grid_res_f > self.xseep;
            if nn_right != self.on_right {
                self.dec_near(self.on_right as i32, 0, self.cell_e_travel);
                self.dec_near(
                    self.on_right as i32,
                    self.on_above as i32,
                    self.cell_e_travel,
                );
                self.on_right = nn_right;
                self.inc_near(self.on_right as i32, 0, self.cell_s_travel);
                self.inc_near(
                    self.on_right as i32,
                    self.on_above as i32,
                    self.cell_s_travel,
                );
            }
        }
        if self.sz == self.nz {
            let nn_above = self.land.z - self.nz as f32 * self.grid_res_f > self.zseep;
            if nn_above != self.on_above {
                self.dec_near(0, self.on_above as i32, self.cell_e_travel);
                self.dec_near(
                    self.on_right as i32,
                    self.on_above as i32,
                    self.cell_e_travel,
                );
                self.on_above = nn_above;
                self.inc_near(0, self.on_above as i32, self.cell_s_travel);
                self.inc_near(
                    self.on_right as i32,
                    self.on_above as i32,
                    self.cell_s_travel,
                );
            }
        }

        // set etravel if there's no next cell
        if self.nx == self.sx && self.nz == self.sz {
            self.cell_e_travel = self.full_dist;

            // remove the four corners
            //  intentionally after sx == nx, etc. tests above
            self.dec_near(0, 0, self.cell_e_travel);
            self.dec_near(0, self.on_above as i32, self.cell_e_travel);
            self.dec_near(self.on_right as i32, 0, self.cell_e_travel);
            self.dec_near(
                self.on_right as i32,
                self.on_above as i32,
                self.cell_e_travel,
            );

            // and don't come back!
            self.all_over = true;
        }

        self.cell_e_travel_private = self.cell_e_travel;
    }

    /// `popAltCell` (`grid_traversal.hpp:373-386`).
    fn pop_alt_cell(&mut self) -> bool {
        if self.alt_cells.is_empty() {
            return false;
        }

        let cs = self.alt_cells.pop().unwrap();

        self.sx = cs.x;
        self.sz = cs.z;
        self.cell_s_travel = cs.s_travel;
        self.cell_e_travel = cs.e_travel;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(x: f32, y: f32, z: f32) -> Vector3 {
        Vector3::new(x, y, z)
    }

    /// One collide-loop walk: the cell answers in visit order.
    fn walk(src: Vector3, dst: Vector3, range: Vector3, grid: f32) -> Vec<(i32, i32, f32, f32)> {
        let mut t = SpaceGridTraversal::new(src, dst, range, grid);
        let mut seen = Vec::new();
        loop {
            seen.push((t.sx, t.sz, t.cell_s_travel, t.cell_e_travel));
            if !t.next() {
                break;
            }
        }
        assert!(
            t.all_over(),
            "the walk ended allOver at ({}, {})",
            t.sx,
            t.sz
        );
        seen
    }

    fn assert_walk(seen: &[(i32, i32, f32, f32)], expected: &[(i32, i32, f32, f32)]) {
        assert_eq!(
            seen.len(),
            expected.len(),
            "cell count: seen {seen:?} expected {expected:?}"
        );
        for (i, (s, e)) in seen.iter().zip(expected).enumerate() {
            assert_eq!(s.0, e.0, "cell {i} sx");
            assert_eq!(s.1, e.1, "cell {i} sz");
            assert!(
                (s.2 - e.2).abs() < 1e-5,
                "cell {i} sTravel {} vs {}",
                s.2,
                e.2
            );
            assert!(
                (s.3 - e.3).abs() < 1e-5,
                "cell {i} eTravel {} vs {}",
                s.3,
                e.3
            );
        }
    }

    /// The reference `dump_sgt` oracle's exact cell sequences.
    // The literals below are the reference program's printed walk,
    // pinned verbatim: precision and const-shape lints are exempt.
    #[allow(clippy::approx_constant, clippy::excessive_precision)]
    #[test]
    fn axis_and_diagonal_walks_match_the_reference_dump() {
        let zero = v(0.0, 0.0, 0.0);

        // [plusx]
        assert_walk(
            &walk(v(0.5, 0.0, 0.5), v(3.5, 0.0, 0.5), zero, 1.0),
            &[
                (0, 0, 0.000000, 0.500000),
                (1, 0, 0.500000, 1.500000),
                (2, 0, 1.500000, 2.500000),
                (3, 0, 2.500000, 3.000000),
            ],
        );

        // [diag] — the first cell is the ctor's popped alternate, not the
        // source cell; (0,0) arrives second.
        assert_walk(
            &walk(v(0.5, 0.0, 0.5), v(3.5, 0.0, 3.5), zero, 1.0),
            &[
                (1, 0, 0.707107, 0.707107),
                (0, 0, 0.000000, 0.707107),
                (2, 1, 2.121320, 2.121320),
                (1, 1, 0.707107, 2.121320),
                (2, 2, 2.121320, 3.535534),
                (3, 2, 3.535534, 3.535534),
                (3, 3, 3.535534, 4.242640),
            ],
        );

        // [revdiag]
        assert_walk(
            &walk(v(3.5, 0.0, 3.5), v(0.5, 0.0, 0.5), zero, 1.0),
            &[
                (2, 3, 0.707107, 0.707107),
                (3, 3, 0.000000, 0.707107),
                (1, 2, 2.121320, 2.121320),
                (2, 2, 0.707107, 2.121320),
                (1, 1, 2.121320, 3.535534),
                (0, 1, 3.535534, 3.535534),
                (0, 0, 3.535534, 4.242640),
            ],
        );

        // [anti]
        assert_walk(
            &walk(v(0.5, 0.0, 3.5), v(3.5, 0.0, 0.5), zero, 1.0),
            &[
                (1, 3, 0.707107, 0.707107),
                (0, 3, 0.000000, 0.707107),
                (2, 2, 2.121320, 2.121320),
                (1, 2, 0.707107, 2.121320),
                (2, 1, 2.121320, 3.535534),
                (3, 1, 3.535534, 3.535534),
                (3, 0, 3.535534, 4.242640),
            ],
        );

        // [anti_rev]
        assert_walk(
            &walk(v(3.5, 0.0, 0.5), v(0.5, 0.0, 3.5), zero, 1.0),
            &[
                (2, 0, 0.707107, 0.707107),
                (3, 0, 0.000000, 0.707107),
                (1, 1, 2.121320, 2.121320),
                (2, 1, 0.707107, 2.121320),
                (1, 2, 2.121320, 3.535534),
                (0, 2, 3.535534, 3.535534),
                (0, 3, 3.535534, 4.242640),
            ],
        );
    }

    // The literals below are the reference program's printed walk,
    // pinned verbatim: precision and const-shape lints are exempt.
    #[allow(clippy::approx_constant, clippy::excessive_precision)]
    #[test]
    fn negative_and_degenerate_walks_match_the_reference_dump() {
        let zero = v(0.0, 0.0, 0.0);

        // [negstart]
        assert_walk(
            &walk(v(-1.5, 0.0, -0.5), v(1.5, 0.0, 1.5), zero, 1.0),
            &[
                (-2, -1, 0.000000, 0.600925),
                (-1, -1, 0.600925, 0.901388),
                (-1, 0, 0.901388, 1.802776),
                (0, 0, 1.802776, 2.704164),
                (0, 1, 2.704164, 3.004626),
                (1, 1, 3.004626, 3.605551),
            ],
        );

        // [endneg]
        assert_walk(
            &walk(v(0.5, 0.0, 0.5), v(-2.5, 0.0, -0.5), zero, 1.0),
            &[
                (0, 0, 0.000000, 0.527046),
                (-1, 0, 0.527046, 1.581139),
                (-1, -1, 1.581139, 1.581139),
                (-2, -1, 1.581139, 2.635231),
                (-3, -1, 2.635231, 3.162278),
            ],
        );

        // [incell] — one cell, eTravel = fullDist.
        assert_walk(
            &walk(v(0.2, 0.0, 0.2), v(0.8, 0.0, 0.8), zero, 1.0),
            &[(0, 0, 0.000000, 0.848528)],
        );

        // [edge] — a source exactly on the grid edge.
        assert_walk(
            &walk(v(1.0, 0.0, 0.5), v(3.0, 0.0, 0.5), zero, 1.0),
            &[(1, 0, 0.000000, 1.000000), (2, 0, 1.000000, 2.000000)],
        );

        // [downz]
        assert_walk(
            &walk(v(2.5, 0.0, 2.5), v(2.5, 0.0, -1.5), zero, 1.0),
            &[
                (2, 2, 0.000000, 0.500000),
                (2, 1, 0.500000, 1.500000),
                (2, 0, 1.500000, 2.500000),
                (2, -1, 2.500000, 3.500000),
                (2, -2, 3.500000, 4.000000),
            ],
        );
    }

    // The literals below are the reference program's printed walk,
    // pinned verbatim: precision and const-shape lints are exempt.
    #[allow(clippy::approx_constant, clippy::excessive_precision)]
    #[test]
    fn a_coarser_grid_walk_matches_the_reference_dump() {
        // [grid2]
        assert_walk(
            &walk(v(0.5, 0.0, 0.5), v(7.5, 0.0, 3.5), v(0.0, 0.0, 0.0), 2.0),
            &[
                (0, 0, 0.000000, 1.631951),
                (2, 0, 3.807886, 3.807886),
                (1, 0, 1.631951, 3.807886),
                (2, 1, 3.807886, 5.983821),
                (3, 1, 5.983821, 7.615773),
            ],
        );
    }

    // The literals below are the reference program's printed walk,
    // pinned verbatim: precision and const-shape lints are exempt.
    #[allow(clippy::approx_constant, clippy::excessive_precision)]
    #[test]
    fn wide_shapes_queue_side_and_overshoot_cells() {
        // [wide] — the near set's side cells interleave, and the end's
        // corners queue cells past the extent.
        assert_walk(
            &walk(v(0.5, 0.0, 0.5), v(3.5, 0.0, 3.5), v(0.6, 0.0, 0.6), 1.0),
            &[
                (1, 0, 0.000000, 0.707107),
                (0, 1, 0.000000, 0.707107),
                (0, 0, 0.000000, 0.707107),
                (2, 1, 2.121320, 2.121320),
                (1, 1, 0.000000, 2.121320),
                (2, 2, 2.121320, 3.535534),
                (3, 2, 3.535534, 3.535534),
                (4, 4, 3.535534, 4.242640),
                (4, 3, 3.535534, 4.242640),
                (3, 4, 3.535534, 4.242640),
                (3, 3, 2.121320, 4.242640),
            ],
        );

        // [wide_x] — range.x over the grid res keeps onRight stuck on.
        assert_walk(
            &walk(v(0.5, 0.0, 0.5), v(3.5, 0.0, 1.5), v(1.5, 0.0, 0.5), 1.0),
            &[
                (0, 0, 0.000000, 0.527046),
                (2, 0, 1.581139, 1.581139),
                (1, 1, 0.000000, 1.581139),
                (1, 0, 0.000000, 1.581139),
                (2, 1, 1.581139, 2.635231),
                (4, 1, 2.635231, 3.162278),
                (3, 1, 2.635231, 3.162278),
            ],
        );
    }
}
