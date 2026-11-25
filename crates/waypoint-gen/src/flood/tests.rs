//! Fixtures and tests for the flood port. The physics seam is a
//! synthetic scene of horizontal slabs (axis-aligned rectangles with a
//! height); walkability is a flat `max_step` rule, so every expected
//! grid below is hand-computable.

use super::*;
use foundation::math::vector::Vector3;

/// A horizontal surface: `x` in `[x0,x1)`, `z` in `[z0,z1)`, at `height`.
pub(crate) struct Slab(
    pub(crate) f32,
    pub(crate) f32,
    pub(crate) f32,
    pub(crate) f32,
    pub(crate) f32,
);

/// Synthetic `IPhysics` over a slab list, mirroring the seam contract of
/// `tools/common/physics_handler.cpp` (topmost surface at or below the
/// query point; impossible moves signalled with `dst2.y = -10000`). The
/// handler's `+0.01` drop fudge belongs to that handler, not to the
/// flood algorithm, and is left out here.
pub(crate) struct FieldPhysics {
    pub(crate) slabs: Vec<Slab>,
    pub(crate) max_step: f32,
}

impl FieldPhysics {
    pub(crate) fn top_at(&self, x: f32, z: f32, below: f32) -> Option<f32> {
        self.slabs
            .iter()
            .filter(|s| x >= s.0 && x < s.2 && z >= s.1 && z < s.3 && s.4 <= below)
            .map(|s| s.4)
            .fold(None, |best: Option<f32>, h| {
                Some(best.map_or(h, |b: f32| b.max(h)))
            })
    }
}

impl FloodPhysics for FieldPhysics {
    fn get_girth(&self) -> Vector3 {
        Vector3::new(0.5, 1.5, 0.5)
    }

    fn get_scramble_height(&self) -> f32 {
        0.5
    }

    fn find_drop_point(&mut self, pos: &Vector3, y: &mut f32) -> bool {
        match self.top_at(pos.x, pos.z, pos.y) {
            Some(h) => {
                *y = h;
                true
            }
            None => false,
        }
    }

    fn adjust_move(&mut self, src: &Vector3, dst: &Vector3, dst2: &mut Vector3) {
        // applyGravity raises the position by hopMax before dropping
        // (`physics_handler.cpp:221`)
        match self.top_at(dst.x, dst.z, dst.y + self.get_scramble_height()) {
            Some(h) if (h - src.y).abs() <= self.max_step => {
                *dst2 = Vector3::new(dst.x, h, dst.z);
            }
            _ => {
                // "cannot find bottom, mark it as invalid"
                // (`physics_handler.cpp:328,337`)
                *dst2 = Vector3::new(dst.x, -10000.0, dst.z);
            }
        }
    }
}

/// A progress callback that never asks to stop.
pub(crate) struct Never;
impl FloodProgress for Never {
    fn filled(&mut self, _npoints: i32) -> bool {
        false
    }
}

/// Synthetic chunk: an ownership box plus the space bounds the flood
/// probes (`waypoint_flood.cpp:537-543`).
pub(crate) struct Bounds {
    pub(crate) owns_box: (f32, f32, f32, f32),
    pub(crate) space: (f32, f32, f32, f32),
}

impl Bounds {
    pub(crate) fn everywhere() -> Self {
        Self {
            owns_box: (-100.0, -100.0, 100.0, 100.0),
            space: (-100.0, 100.0, -100.0, 100.0),
        }
    }
}

impl FloodChunk for Bounds {
    fn owns(&self, point: &Vector3) -> bool {
        let (x0, z0, x1, z1) = self.owns_box;
        point.x >= x0 && point.x <= x1 && point.z >= z0 && point.z <= z1
    }
    fn min_coord_x(&self) -> f32 {
        self.space.0
    }
    fn max_coord_x(&self) -> f32 {
        self.space.1
    }
    fn min_coord_z(&self) -> f32 {
        self.space.2
    }
    fn max_coord_z(&self) -> f32 {
        self.space.3
    }
}

pub(crate) fn flat_ground() -> FieldPhysics {
    FieldPhysics {
        slabs: vec![Slab(-100.0, -100.0, 100.0, 100.0, 0.0)],
        max_step: 0.6,
    }
}

pub(crate) fn grid(min: f32, max: f32, res: f32) -> WaypointFlood {
    let mut flood = WaypointFlood::default();
    flood.set_area(
        &Vector3::new(min, 0.0, min),
        &Vector3::new(max, 0.0, max),
        res,
    );
    flood
}

pub(crate) fn seed(x: f32, y: f32, z: f32) -> Vector3 {
    Vector3::new(x, y, z)
}

/// The angle slot layout, for readable expectations.
pub(crate) const U: usize = 0;
pub(crate) const UR: usize = 1;
pub(crate) const R: usize = 2;
pub(crate) const DR: usize = 3;
pub(crate) const D: usize = 4;
pub(crate) const DL: usize = 5;
pub(crate) const L: usize = 6;
pub(crate) const UL: usize = 7;

#[test]
fn set_area_quantises_and_offsets_the_box() {
    let mut flood = WaypointFlood::default();
    flood.set_area(
        &Vector3::new(0.05, 0.0, 0.05),
        &Vector3::new(4.05, 0.0, 4.05),
        1.0,
    );
    // 0.05 snaps down to 0, 4.05 snaps up to 5; sizes come from the
    // un-quantised parameters ((4.05-0.05)/1+1 = 5); the hack offset
    // lands afterwards
    assert_eq!(flood.min(), &Vector3::new(0.1, 0.0, 0.1));
    assert_eq!(flood.max(), &Vector3::new(5.1, 0.0, 5.1));
    assert_eq!(flood.xsize(), 5);
    assert_eq!(flood.zsize(), 5);
    assert_eq!(flood.resolution(), 1.0);
    // fresh layers start empty and void
    assert_eq!(flood.smallest_height(), MAX_HEIGHTS as i32);
    assert_eq!(flood.hgt_grids()[15][0], -10000.0);
    assert_eq!(flood.adj_grids()[15][0], AdjGridElt { all: 0 });
}

#[test]
fn angle_slots_round_trip() {
    let mut elt = AdjGridElt::default();
    for a in 0..8usize {
        elt.set_angle(a, a as u32 + 1);
    }
    assert_eq!(
        elt.all,
        0x8765_4321, // slot a holds a+1
    );
    for a in 0..8usize {
        assert_eq!(elt.angle(a), a as u32 + 1);
    }
    // the setter only touches its own nibble
    elt.set_angle(3, 9);
    assert_eq!(elt.angle(3), 9);
    assert_eq!(elt.angle(2), 3);
}

#[test]
fn flat_fill_links_the_whole_grid() {
    let mut flood = grid(0.0, 4.0, 1.0);
    let mut physics = flat_ground();
    let chunk = Bounds::everywhere();

    let count = flood.fill(&seed(0.5, 10.0, 0.5), &mut physics, &chunk, None);
    assert_eq!(count, 25); // 5x5 cells, each popped once

    // every cell sits on layer 15 at height 0
    for i in 0..25 {
        assert_eq!(flood.hgt_grids()[15][i], 0.0);
    }
    assert_eq!(flood.smallest_height(), 15);

    // corner (0,0): north, northeast and east exist; the rest are void
    let corner = flood.adj_grids()[15][0];
    assert_eq!(corner.angle(U), 15);
    assert_eq!(corner.angle(UR), 15);
    assert_eq!(corner.angle(R), 15);
    assert_eq!(corner.angle(DR), 0);
    assert_eq!(corner.angle(D), 0);
    assert_eq!(corner.angle(DL), 0);
    assert_eq!(corner.angle(L), 0);
    assert_eq!(corner.angle(UL), 0);
    assert_eq!(corner.all, 15 | (15 << 4) | (15 << 8));

    // centre (2,2) is fully linked
    assert_eq!(flood.adj_grids()[15][12].all, 0xffffffff);

    // opposite corner trims the mirror set
    assert_eq!(
        flood.adj_grids()[15][24].all,
        (15 << 16) | (15 << 20) | (15 << 24)
    );

    // links are reciprocated
    assert_eq!(flood.adj_grids()[15][1].angle(L), 15);
    assert!(flood.check_grid_consistency());
}

#[test]
fn slope_steps_still_flood_one_layer() {
    // ground rises 0.5 per +z row: every step is walkable, so the whole
    // 5x5 grid floods into layer 15 with the ramp recorded as heights
    let mut flood = grid(0.0, 4.0, 1.0);
    let mut physics = FieldPhysics {
        slabs: vec![],
        max_step: 0.6,
    };
    for z in 0..5i32 {
        physics.slabs.push(Slab(
            -100.0,
            z as f32,
            100.0,
            (z + 1) as f32,
            0.5 * z as f32,
        ));
    }

    let chunk = Bounds::everywhere();
    let count = flood.fill(&seed(0.5, 10.0, 0.5), &mut physics, &chunk, None);
    assert_eq!(count, 25);

    for z in 0..5i32 {
        for x in 0..5i32 {
            let i = (x + z * 5) as usize;
            assert_eq!(flood.hgt_grids()[15][i], 0.5 * z as f32, "cell ({x},{z})");
        }
    }
    assert_eq!(flood.adj_grids()[15][12].all, 0xffffffff);
    assert!(flood.check_grid_consistency());
}

#[test]
fn a_slope_too_steep_to_walk_leaves_nothing() {
    // max_step 0.4 against 0.5 steps: no move passes adjustMove, the
    // seed cell pops, links nothing, and its height is rolled back
    let mut flood = grid(0.0, 4.0, 1.0);
    let mut physics = FieldPhysics {
        slabs: vec![],
        max_step: 0.4,
    };
    // every cell is its own step: any 8-neighbour move climbs 0.5 (1.0
    // diagonally), past max_step
    for x in 0..5i32 {
        for z in 0..5i32 {
            physics.slabs.push(Slab(
                x as f32,
                z as f32,
                (x + 1) as f32,
                (z + 1) as f32,
                0.5 * (x + z) as f32,
            ));
        }
    }

    let chunk = Bounds::everywhere();
    let count = flood.fill(&seed(0.5, 10.0, 0.5), &mut physics, &chunk, None);
    assert_eq!(count, 1);
    // the seed's height reservation is undone
    assert_eq!(flood.hgt_grids()[15][0], -10000.0);
    assert_eq!(flood.adj_grids()[15][0].all, 0);
}

#[test]
fn indoor_outdoor_seam_keeps_the_two_floors_unlinked() {
    // outdoor ground at 0 for z<3, an indoor floor at 2 for z>=3; the
    // 2-unit wall between them exceeds max_step, so both regions flood
    // (separate seeds) but never link across the seam
    let mut flood = grid(0.0, 4.0, 1.0);
    let mut physics = FieldPhysics {
        slabs: vec![
            Slab(-100.0, -100.0, 100.0, 3.0, 0.0),
            Slab(-100.0, 3.0, 100.0, 100.0, 2.0),
        ],
        max_step: 0.6,
    };
    let chunk = Bounds::everywhere();

    let out = flood.fill(&seed(0.5, 10.0, 0.5), &mut physics, &chunk, None);
    assert_eq!(out, 15); // rows z=0,1,2
    let inn = flood.fill(&seed(2.5, 10.0, 3.5), &mut physics, &chunk, None);
    assert_eq!(inn, 10); // rows z=3,4

    for x in 0..5i32 {
        for z in 0..5i32 {
            let i = (x + z * 5) as usize;
            let want = if z < 3 { 0.0 } else { 2.0 };
            assert_eq!(flood.hgt_grids()[15][i], want, "cell ({x},{z})");
        }
    }
    // the seam: no link in either direction across z=2 -> z=3
    assert_eq!(flood.adj_grids()[15][12].angle(U), 0); // (2,2) north
    assert_eq!(flood.adj_grids()[15][17].angle(D), 0); // (2,3) south
    // away from the seam the floors are whole
    assert_eq!(flood.adj_grids()[15][6].angle(U), 15); // (1,1) north
    assert_eq!(flood.adj_grids()[15][18].angle(U), 15); // (3,3) north
    assert!(flood.check_grid_consistency());
}

#[test]
fn a_roof_stacks_two_layers_per_cell() {
    // roof at height 3 over world x in [1,3), i.e. grid columns 1,2;
    // walkable both on top of it and underneath. The first fill covers
    // the roof, the other two cover the ground, stacking layer 14 under
    // layer 15 on the roofed cells.
    let mut flood = grid(0.0, 4.0, 1.0);
    let mut physics = FieldPhysics {
        slabs: vec![
            Slab(-100.0, -100.0, 100.0, 100.0, 0.0),
            Slab(1.0, -100.0, 3.0, 100.0, 3.0),
        ],
        max_step: 0.6,
    };
    let chunk = Bounds::everywhere();

    let roof = flood.fill(&seed(2.5, 10.0, 2.5), &mut physics, &chunk, None);
    assert_eq!(roof, 10); // roof columns x=1,2
    assert_eq!(flood.smallest_height(), 15);
    let left = flood.fill(&seed(0.5, 10.0, 2.5), &mut physics, &chunk, None);
    // the west seed stacks layer 14 under the roof on columns 1,2 and
    // then walks the ground out the far side: all 25 cells pop
    assert_eq!(left, 25);
    // the east seed's cell is already filled on its layer 15
    let right = flood.fill(&seed(3.5, 10.0, 2.5), &mut physics, &chunk, None);
    assert_eq!(right, 0);

    assert_eq!(flood.smallest_height(), 14);

    // the roofed cell (2,2): layer 15 is the roof, layer 14 the floor
    let i = (2 + 2 * 5) as usize;
    assert_eq!(flood.hgt_grids()[15][i], 3.0);
    assert_eq!(flood.hgt_grids()[14][i], 0.0);
    // the stacked floor links west through layer 14 to column 1's stack
    assert_eq!(flood.adj_grids()[14][i].angle(L), 14);
    // the roof layer on column 3's neighbour links into the stack
    assert_eq!(flood.adj_grids()[15][(3 + 2 * 5) as usize].angle(L), 14);
    // the west ground linked into the stack the same way
    assert_eq!(flood.hgt_grids()[14][11], 0.0); // (1,2)
    assert_eq!(flood.adj_grids()[14][(1 + 2 * 5) as usize].angle(L), 15);
    assert_eq!(flood.adj_grids()[15][10].angle(R), 14); // (0,2)
    // the roof layer keeps its own south link
    assert_eq!(flood.adj_grids()[15][i].angle(D), 15);
    // the un-roofed cells never stack
    assert_eq!(flood.hgt_grids()[14][13], -10000.0); // (3,2)
    assert_eq!(flood.hgt_grids()[14][10], -10000.0); // (0,2)
    assert!(flood.check_grid_consistency());
}

#[test]
fn a_tight_chunk_records_boundary_points() {
    // the chunk owns nothing past x=0.5. checkBounds probes one cell
    // sideways, so the move from column 0 into column 1 is still allowed
    // (both probed points land inside); column 1 -> column 2 has no
    // owning probe pair and lands in the boundary set.
    let mut flood = grid(0.0, 4.0, 1.0);
    let mut physics = flat_ground();
    let chunk = Bounds {
        owns_box: (-100.0, -100.0, 0.5, 100.0),
        space: (-100.0, 100.0, -100.0, 100.0),
    };

    let count = flood.fill(&seed(0.5, 10.0, 2.5), &mut physics, &chunk, None);
    assert_eq!(count, 10); // columns x=0,1 flood, column 2 never

    for z in 0..5i32 {
        let i = z * 5;
        // columns 0 and 1 flooded
        assert_eq!(flood.hgt_grids()[15][(i + 1) as usize], 0.0, "row {z}");
        // column 1's eastward height stayed void and no east link formed
        assert_eq!(flood.hgt_grids()[15][(i + 2) as usize], -10000.0, "row {z}");
        assert_eq!(flood.adj_grids()[15][(i + 1) as usize].angle(R), 0);
        // column 0's east link into column 1 exists
        assert_eq!(flood.adj_grids()[15][i as usize].angle(R), 15);
        assert!(
            flood.boundary_set().contains(&(i + 1, 15)),
            "cell (1,{z}) is a boundary point"
        );
    }
    assert_eq!(flood.boundary_set().len(), 5);
}

#[test]
fn filling_an_already_filled_cell_reports_zero() {
    let mut flood = grid(0.0, 2.0, 1.0);
    let mut physics = flat_ground();
    let chunk = Bounds::everywhere();

    assert_eq!(
        flood.fill(&seed(0.5, 10.0, 0.5), &mut physics, &chunk, None),
        9
    );
    // the seed cell now has adjacencies: a second fill refuses
    assert_eq!(
        flood.fill(&seed(0.5, 10.0, 0.5), &mut physics, &chunk, None),
        0
    );
}

#[test]
fn a_seed_that_cannot_drop_or_fit_fails() {
    let mut flood = grid(0.0, 2.0, 1.0);
    let mut physics = flat_ground();
    let chunk = Bounds::everywhere();

    // bottomless void: ground lookup misses, with a live progress
    // (the reference warning is gated on exactly that)
    let mut void = FieldPhysics {
        slabs: vec![Slab(50.0, 50.0, 60.0, 60.0, 0.0)],
        max_step: 0.6,
    };
    let mut never = Never;
    assert_eq!(
        flood.fill(&seed(0.5, 10.0, 0.5), &mut void, &chunk, Some(&mut never)),
        0
    );

    // a seed far outside the chunk exhausts the checkBounds cascade
    assert_eq!(
        flood.fill(&seed(95.5, 10.0, 0.5), &mut physics, &chunk, None),
        0
    );

    // a seed that rounds outside the grid errors out: use a chunk whose
    // cascade shifts cannot rescue the round
    let tiny = Bounds {
        owns_box: (-100.0, -100.0, -50.0, 100.0),
        space: (-100.0, 100.0, -100.0, 100.0),
    };
    assert_eq!(
        flood.fill(&seed(-45.5, 10.0, 2.5), &mut physics, &tiny, None),
        0
    );
}

#[test]
fn progress_can_stop_the_flood() {
    let mut flood = grid(0.0, 14.0, 1.0); // 15x15 = 225 cells
    let mut physics = flat_ground();
    let chunk = Bounds::everywhere();

    struct Stop;
    impl FloodProgress for Stop {
        fn filled(&mut self, npoints: i32) -> bool {
            npoints == 200
        }
    }
    let result = flood.fill(&seed(0.5, 10.0, 0.5), &mut physics, &chunk, Some(&mut Stop));
    // the callback returns true exactly once, at the 200th pop, and the
    // flood reports the stop
    assert_eq!(result, -1);

    // a progress that never stops leaves the flood complete
    let mut flood2 = grid(0.0, 14.0, 1.0);
    pub(crate) struct Never;
    impl FloodProgress for Never {
        fn filled(&mut self, _n: i32) -> bool {
            false
        }
    }
    let result = flood2.fill(
        &seed(0.5, 10.0, 0.5),
        &mut physics,
        &chunk,
        Some(&mut Never),
    );
    assert_eq!(result, 225);
}

#[test]
fn flash_flood_covers_the_grid_and_trims_edges() {
    let mut flood = grid(0.0, 3.0, 1.0); // 4x4
    flood.flash_flood(5.0);

    // there is no untouched interior cell in a 4x4 grid; cell 5 is the
    // diagonal neighbour of corner (0,0), whose corner removal clears
    // its dl slot
    assert_eq!(flood.hgt_grids()[15][5], 5.0);
    assert_eq!(flood.adj_grids()[15][5].all, 0xff0f_ffff);

    // bottom row loses south/southwest/southeast (dr,d,dl = 3,4,5)
    let a = flood.adj_grids()[15][1].all;
    assert_eq!(a & (15 << 12), 0);
    assert_eq!(a & (15 << 16), 0);
    assert_eq!(a & (15 << 20), 0);
    assert_eq!(a & 15, 15);
    // top row loses north/northeast/northwest (u,ur,ul = 0,1,7)
    let b = flood.adj_grids()[15][14].all;
    assert_eq!(b & 15, 0);
    assert_eq!(b & (15 << 4), 0);
    assert_eq!(b & (15 << 28), 0);
    // left column loses dl,l,ul (5,6,7)
    let c = flood.adj_grids()[15][4].all;
    assert_eq!(c & (15 << 20), 0);
    assert_eq!(c & (15 << 24), 0);
    assert_eq!(c & (15 << 28), 0);
    // right column loses ur,r,dr (1,2,3)
    let d = flood.adj_grids()[15][7].all;
    assert_eq!(d & (15 << 4), 0);
    assert_eq!(d & (15 << 8), 0);
    assert_eq!(d & (15 << 12), 0);

    // corners are zeroed entirely, and their inward neighbours lose the
    // angle that pointed back at them: a=0 clears (0,1)'s d, (1,1)'s dl
    // and (1,0)'s l
    assert_eq!(flood.adj_grids()[15][0].all, 0);
    assert_eq!(flood.adj_grids()[15][15].all, 0);
    assert_eq!(flood.adj_grids()[15][3].all, 0);
    assert_eq!(flood.adj_grids()[15][12].all, 0);
    // a=0 clears (0,1)'s d, (1,1)'s dl and (1,0)'s l
    assert_eq!(flood.adj_grids()[15][4].angle(D), 0);
    assert_eq!(flood.adj_grids()[15][5].angle(DL), 0);
    assert_eq!(flood.adj_grids()[15][1].angle(L), 0);
    // a=2 clears (1,3)'s l, (1,2)'s ul and (0,2)'s u
    assert_eq!(flood.adj_grids()[15][13].angle(L), 0);
    assert_eq!(flood.adj_grids()[15][9].angle(UL), 0);
    assert_eq!(flood.adj_grids()[15][8].angle(U), 0);
    // a=4 clears (3,2)'s u, (2,2)'s ur and (2,3)'s r
    assert_eq!(flood.adj_grids()[15][11].angle(U), 0);
    assert_eq!(flood.adj_grids()[15][10].angle(UR), 0);
    assert_eq!(flood.adj_grids()[15][14].angle(R), 0);
    // a=6 clears (2,0)'s r, (2,1)'s dr and (3,1)'s d
    assert_eq!(flood.adj_grids()[15][2].angle(R), 0);
    assert_eq!(flood.adj_grids()[15][6].angle(DR), 0);
    assert_eq!(flood.adj_grids()[15][7].angle(D), 0);

    // flash flooding does not touch the smallest-height cursor
    assert_eq!(flood.smallest_height(), MAX_HEIGHTS as i32);
}

#[test]
fn postfilter_add_bridges_a_single_missing_link() {
    let mut flood = grid(0.0, 2.0, 1.0); // 3x3
    let mut physics = flat_ground();
    let chunk = Bounds::everywhere();
    flood.fill(&seed(0.5, 10.0, 0.5), &mut physics, &chunk, None);
    assert_eq!(flood.smallest_height(), 15);

    // reduce the layer to: centre linked north only, north cell linked
    // southwest (reciprocated) — the smoothing pass bridges centre's
    // missing west link through the north cell
    let layer = &mut flood.adj_grids_mut()[15];
    for elt in layer.iter_mut() {
        elt.all = 0;
    }
    layer[4].set_angle(U, 15);
    layer[7].set_angle(D, 15);
    layer[7].set_angle(DL, 15);
    layer[3].set_angle(UR, 15);

    flood.postfilter_add();

    // the west link appeared on both sides
    assert_eq!(flood.adj_grids()[15][4].angle(L), 15);
    assert_eq!(flood.adj_grids()[15][3].angle(R), 15);
    // no other link was invented (centre's east stays missing: the north
    // cell has no southeast link to bridge through)
    assert_eq!(flood.adj_grids()[15][4].angle(R), 0);
    // the diagonal arm found no full square, so nothing else changed
    assert_eq!(flood.adj_grids()[15][4].angle(U), 15);
    assert_eq!(flood.adj_grids()[15][7].angle(DL), 15);
    assert!(flood.check_grid_consistency());
}

#[test]
fn postfilter_add_fills_a_square_diagonal() {
    let mut flood = grid(0.0, 2.0, 1.0); // 3x3
    let mut physics = flat_ground();
    let chunk = Bounds::everywhere();
    flood.fill(&seed(0.5, 10.0, 0.5), &mut physics, &chunk, None);

    // four straight links forming the square (0,0)-(1,0)-(0,1)-(1,1),
    // all on layer 15, all reciprocated, no diagonals
    let layer = &mut flood.adj_grids_mut()[15];
    for elt in layer.iter_mut() {
        elt.all = 0;
    }
    layer[4].set_angle(L, 15); // (1,1) west
    layer[4].set_angle(D, 15); // (1,1) south
    layer[3].set_angle(R, 15); // (0,1) east
    layer[3].set_angle(D, 15); // (0,1) south
    layer[1].set_angle(U, 15); // (1,0) north
    layer[1].set_angle(L, 15); // (1,0) west
    layer[0].set_angle(U, 15); // (0,0) north
    layer[0].set_angle(R, 15); // (0,0) east

    flood.postfilter_add();

    // the (1,1) southwest diagonal and its three mirror angles
    assert_eq!(flood.adj_grids()[15][4].angle(DL), 15);
    assert_eq!(flood.adj_grids()[15][3].angle(DR), 15);
    assert_eq!(flood.adj_grids()[15][0].angle(UR), 15);
    assert_eq!(flood.adj_grids()[15][1].angle(UL), 15);
    assert!(flood.check_grid_consistency());
}

#[test]
fn postfilter_remove_cuts_a_concave_corner() {
    let mut flood = grid(0.0, 2.0, 1.0); // 3x3
    let mut physics = flat_ground();
    let chunk = Bounds::everywhere();
    flood.fill(&seed(0.5, 10.0, 0.5), &mut physics, &chunk, None);

    // the reference's rotated concave-corner shape, hand-built for a=0
    // (`waypoint_flood.cpp:996-1040`): the centre (1,1) holds west,
    // south and southwest links but no dr/ur/ul; the west cell holds
    // east, southeast, south and north links but no ur; the south cell
    // holds north, ul, northwest and west links but no ur
    let layer = &mut flood.adj_grids_mut()[15];
    for elt in layer.iter_mut() {
        elt.all = 0;
    }
    layer[4].set_angle(L, 15); // centre west
    layer[4].set_angle(D, 15); // centre south
    layer[4].set_angle(DL, 15); // centre southwest
    layer[3].set_angle(R, 15); // west cell east
    layer[3].set_angle(DR, 15); // west cell southeast
    layer[3].set_angle(D, 15); // west cell south
    layer[3].set_angle(U, 15); // west cell north (the disambiguating arm)
    layer[1].set_angle(U, 15); // south cell north
    layer[1].set_angle(UL, 15); // south cell northwest
    layer[1].set_angle(L, 15); // south cell west

    flood.postfilter_remove();

    // the centre point is removed wholesale
    assert_eq!(flood.adj_grids()[15][4].all, 0);
    // and its mirror angles are cleared on the touched neighbours
    assert_eq!(flood.adj_grids()[15][1].angle(U), 0); // south cell north
    assert_eq!(flood.adj_grids()[15][1].angle(UR), 0); // (was unset)
    assert_eq!(flood.adj_grids()[15][0].angle(UR), 0);
    // the west cell loses east and northeast but keeps north
    assert_eq!(flood.adj_grids()[15][3].angle(R), 0);
    assert_eq!(flood.adj_grids()[15][3].angle(UR), 0);
    assert_eq!(flood.adj_grids()[15][3].angle(U), 15);
}

#[test]
fn consistency_check_rejects_a_one_way_link() {
    let mut flood = grid(0.0, 2.0, 1.0);
    let mut physics = flat_ground();
    let chunk = Bounds::everywhere();
    flood.fill(&seed(0.5, 10.0, 0.5), &mut physics, &chunk, None);
    assert!(flood.check_grid_consistency());

    // sever the reciprocal of (1,1)'s east link
    flood.adj_grids_mut()[15][5].set_angle(L, 0);
    assert!(!flood.check_grid_consistency());
}

#[test]
fn shrink_is_a_reference_no_op() {
    // the entire body of `WaypointFlood::shrink` is `#if 0`-disabled
    let mut flood = grid(0.0, 2.0, 1.0);
    let mut physics = flat_ground();
    let chunk = Bounds::everywhere();
    flood.fill(&seed(0.5, 10.0, 0.5), &mut physics, &chunk, None);
    let before = flood.adj_grids()[15].to_vec();
    flood.shrink();
    assert_eq!(flood.adj_grids()[15], before);
}

#[test]
fn write_tga_matches_the_reference_layout() {
    let mut flood = grid(0.0, 2.0, 1.0); // 3x3
    let mut physics = flat_ground();
    let chunk = Bounds::everywhere();
    flood.fill(&seed(0.5, 10.0, 0.5), &mut physics, &chunk, None);

    let path = std::env::temp_dir().join("waypoint_gen_test_write.tga");
    assert!(flood.write_tga(path.to_str().unwrap()));
    let bytes = std::fs::read(&path).unwrap();

    // header: 13 bytes of extra data, then the TGA fields with the
    // reshaped dimensions (xsize*MAX_HEIGHTS*4 = 3*16*4 = 192)
    assert_eq!(
        &bytes[0..18],
        &[16, 1, 1, 0, 0, 0, 1, 24, 0, 0, 0, 0, 192, 0, 192, 0, 8, 0]
    );
    // min (0.1, 0, 0.1) and resolution 1.0, little-endian f32s
    assert_eq!(bytes[18..22], 0.1f32.to_le_bytes());
    assert_eq!(bytes[22..26], 0.0f32.to_le_bytes());
    assert_eq!(bytes[26..30], 0.1f32.to_le_bytes());
    assert_eq!(bytes[30..34], 1.0f32.to_le_bytes());
    // 256-entry grayscale colour map
    for i in 0..256usize {
        assert_eq!(bytes[34 + i * 3], i as u8);
        assert_eq!(bytes[34 + i * 3 + 1], i as u8);
        assert_eq!(bytes[34 + i * 3 + 2], i as u8);
    }
    // payload: 16 adjacency layers then 16 height layers, 9 cells each
    let adj_base = 34 + 768;
    let cell = |g: usize, i: usize| adj_base + g * 36 + i * 4..adj_base + g * 36 + i * 4 + 4;
    // corner (0,0) on layer 15: u, ur, r = 15
    assert_eq!(
        bytes[cell(15, 0)],
        (15u32 | (15 << 4) | (15 << 8)).to_le_bytes()
    );
    // centre (1,1) fully linked
    assert_eq!(bytes[cell(15, 4)], 0xffffffffu32.to_le_bytes());
    // layer 0 untouched
    assert_eq!(bytes[cell(0, 4)], 0u32.to_le_bytes());
    let hgt_base = adj_base + 16 * 36;
    assert_eq!(
        bytes[hgt_base + 15 * 36 + 16..hgt_base + 15 * 36 + 20],
        0.0f32.to_le_bytes()
    );
    assert_eq!(bytes.len(), hgt_base + 16 * 36);

    // an unwritable path fails
    assert!(!flood.write_tga("/nonexistent-dir-xyz/wp.tga"));
    std::fs::remove_file(&path).ok();
}

#[test]
fn the_default_is_unblocked_walks_small_height_differences() {
    // exercised through a minimal physics that only defines the drops
    struct Drops(FieldPhysics);
    impl FloodPhysics for Drops {
        fn get_girth(&self) -> Vector3 {
            self.0.get_girth()
        }
        fn get_scramble_height(&self) -> f32 {
            self.0.get_scramble_height()
        }
        fn find_drop_point(&mut self, pos: &Vector3, y: &mut f32) -> bool {
            self.0.find_drop_point(pos, y)
        }
        fn adjust_move(&mut self, src: &Vector3, dst: &Vector3, dst2: &mut Vector3) {
            self.0.adjust_move(src, dst, dst2)
        }
    }
    let inner = FieldPhysics {
        slabs: vec![Slab(-100.0, -100.0, 100.0, 100.0, 0.0)],
        max_step: 0.6,
    };
    let mut physics = Drops(inner);
    let at = |y: f32| Vector3::new(1.0, y, 1.0);

    // same height: trivially unblocked
    assert!(physics.is_unblocked(&at(0.0), 0.0));
    // within the drop fudge of each other
    assert!(physics.is_unblocked(&at(0.0), 0.05));
    // src below the other height, but the drop lands at src's feet
    assert!(physics.is_unblocked(&at(0.0), 0.5));
    // src above the other height, ground at the other height
    assert!(physics.is_unblocked(&at(2.0), 0.0));
    // src below the other height and the floor sits above src: blocked
    assert!(!physics.is_unblocked(&at(-1.0), 0.5));
    // the floor is far above the other height: blocked
    assert!(!physics.is_unblocked(&at(2.0), -50.0));
    // no surface at all below: blocked
    assert!(!physics.is_unblocked(&Vector3::new(1000.0, 2.0, 1.0), 0.0));
    // the reference's fall-through arm: a NaN height matches no
    // comparison and reports unblocked
    assert!(physics.is_unblocked(&Vector3::new(1.0, f32::NAN, 1.0), 0.0));
}
