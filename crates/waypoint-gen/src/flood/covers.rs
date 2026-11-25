//! Degenerate-scene paths of `fill` and the postfilters, driven by
//! hand-built grid state. The organic floods in `tests.rs` reach the
//! common arms; these cover the layer allocation, rollback and stale
//! adjacency fallbacks the reference keeps for scenes the normal flow
//! rarely produces.

use super::tests::*;
use super::*;

/// Chunk whose ownership requires the point to sit near a height
/// mark, so only the scrambled `checkBounds` probes pass.
struct MarkedChunk {
    inner: Bounds,
    mark: f32,
}

impl FloodChunk for MarkedChunk {
    fn owns(&self, point: &Vector3) -> bool {
        self.inner.owns(point) && (point.y - self.mark).abs() <= 0.3
    }
    fn min_coord_x(&self) -> f32 {
        self.inner.min_coord_x()
    }
    fn max_coord_x(&self) -> f32 {
        self.inner.max_coord_x()
    }
    fn min_coord_z(&self) -> f32 {
        self.inner.min_coord_z()
    }
    fn max_coord_z(&self) -> f32 {
        self.inner.max_coord_z()
    }
}

#[test]
fn a_seed_picks_the_closest_matching_layer() {
    // two layers at the seed cell are within the height threshold;
    // the exact match wins and the other stays untouched
    let mut flood = grid(0.0, 2.0, 1.0);
    flood.hgt_grids_mut()[15][0] = 0.0;
    flood.hgt_grids_mut()[14][0] = 0.3;
    let mut physics = flat_ground();
    let chunk = Bounds::everywhere();

    assert_eq!(
        flood.fill(&seed(0.5, 0.0, 0.5), &mut physics, &chunk, None),
        9
    );
    // the seed kept its layer and never re-allocated (the rest of the
    // flood still claims layer 15 on the empty cells)
    assert_eq!(flood.hgt_grids()[14][0], 0.3);
    assert_eq!(flood.smallest_height(), 15);
    assert_eq!(flood.adj_grids()[15][0].angle(R), 15);
}

#[test]
fn a_seed_with_no_room_left_fails() {
    // every layer at the seed cell is taken by heights far from the
    // ground: the reference has no layer left and reports failure
    let mut flood = grid(0.0, 2.0, 1.0);
    for g in 1..MAX_HEIGHTS {
        flood.hgt_grids_mut()[g][0] = 5.0 + 0.7 * g as f32;
    }
    let mut physics = flat_ground();
    let chunk = Bounds::everywhere();

    assert_eq!(
        flood.fill(&seed(0.5, 0.0, 0.5), &mut physics, &chunk, None),
        0
    );
    // nothing was overwritten
    assert_eq!(flood.hgt_grids()[15][0], 5.0 + 0.7 * 15.0);
}

#[test]
fn a_move_picks_the_closest_matching_layer() {
    // the destination offers two layers within the threshold; the
    // exact one wins and the other is left alone
    let mut flood = grid(0.0, 2.0, 1.0);
    flood.hgt_grids_mut()[15][0] = 0.0;
    flood.hgt_grids_mut()[15][1] = 0.0;
    flood.hgt_grids_mut()[14][1] = 0.3;
    let mut physics = flat_ground();
    let chunk = Bounds::everywhere();

    assert_eq!(
        flood.fill(&seed(0.5, 0.0, 0.5), &mut physics, &chunk, None),
        9
    );
    assert_eq!(flood.hgt_grids()[14][1], 0.3);
    assert_eq!(flood.adj_grids()[15][0].angle(R), 15);
}

#[test]
fn a_move_with_no_room_left_is_skipped() {
    // every layer at the destination is taken: the move is skipped
    // and the seed's reservation is rolled back when nothing linked
    let mut flood = grid(0.0, 2.0, 1.0);
    flood.hgt_grids_mut()[15][0] = 0.0;
    for g in 1..MAX_HEIGHTS {
        flood.hgt_grids_mut()[g][1] = 5.0 + 0.7 * g as f32;
    }
    let mut physics = flat_ground();
    let chunk = Bounds::everywhere();

    // the rest of the grid still floods around the blocked cell
    assert_eq!(
        flood.fill(&seed(0.5, 0.0, 0.5), &mut physics, &chunk, None),
        8
    );
    assert_eq!(flood.hgt_grids()[15][1], 5.0 + 0.7 * 15.0);
    assert_eq!(flood.adj_grids()[15][0].angle(R), 0);
}

/// Slab physics whose back-steps against the walking direction fail:
/// any adjustMove whose destination lies west of its source reports
/// no bottom, which is exactly the shape of the second (back) call of
/// an eastward move.
struct OneWayBack {
    inner: FieldPhysics,
}

impl FloodPhysics for OneWayBack {
    fn get_girth(&self) -> Vector3 {
        self.inner.get_girth()
    }
    fn get_scramble_height(&self) -> f32 {
        self.inner.get_scramble_height()
    }
    fn find_drop_point(&mut self, pos: &Vector3, y: &mut f32) -> bool {
        self.inner.find_drop_point(pos, y)
    }
    fn adjust_move(&mut self, src: &Vector3, dst: &Vector3, dst2: &mut Vector3) {
        if dst.x < src.x {
            *dst2 = Vector3::new(dst.x, -10000.0, dst.z);
        } else {
            self.inner.adjust_move(src, dst, dst2);
        }
    }
}

#[test]
fn a_move_whose_back_step_fails_is_skipped() {
    // eastward moves pass the forward adjustMove but their back-step
    // finds no bottom, so they die after the second adjustMove; the
    // north column floods unhindered
    let mut flood = grid(0.0, 2.0, 1.0);
    let mut physics = OneWayBack {
        inner: flat_ground(),
    };
    let chunk = Bounds::everywhere();

    // the girth and scramble seams are unused by the flood itself but
    // part of the trait contract
    assert_eq!(physics.get_girth(), Vector3::new(0.5, 1.5, 0.5));
    assert_eq!(physics.get_scramble_height(), 0.5);
    assert_eq!(
        flood.fill(&seed(0.5, 10.0, 0.5), &mut physics, &chunk, None),
        3
    );
    assert_eq!(flood.adj_grids()[15][0].angle(R), 0);
    assert_eq!(flood.adj_grids()[15][0].angle(U), 15);
    assert!(flood.boundary_set().is_empty());
}

#[test]
fn a_stale_opposite_link_reallocates_the_layer() {
    // the destination cell's layer 15 already carries a west link
    // into layer 14, so the move re-allocates a fresh layer instead
    let mut flood = grid(0.0, 2.0, 1.0);
    flood.hgt_grids_mut()[15][0] = 0.0;
    flood.hgt_grids_mut()[15][1] = 0.0;
    flood.adj_grids_mut()[15][1].set_angle(L, 14);
    flood.hgt_grids_mut()[14][0] = 0.0;
    let mut physics = flat_ground();
    let chunk = Bounds::everywhere();

    let count = flood.fill(&seed(0.5, 0.0, 0.5), &mut physics, &chunk, None);
    assert!(count > 1);
    // the east move landed on the fresh layer 14
    assert_eq!(flood.hgt_grids()[14][1], 0.0);
    assert_eq!(flood.adj_grids()[15][0].angle(R), 14);
    assert_eq!(flood.adj_grids()[14][1].angle(L), 15);
}

#[test]
fn a_stale_opposite_link_with_no_room_skips() {
    // like the reallocation case, but layer 14 at the destination is
    // also taken: the move is skipped without any allocation
    let mut flood = grid(0.0, 2.0, 1.0);
    flood.hgt_grids_mut()[15][0] = 0.0;
    flood.adj_grids_mut()[15][1].set_angle(L, 14);
    // every layer at the destination is taken by heights far from
    // the ground level
    for g in 1..MAX_HEIGHTS {
        flood.hgt_grids_mut()[g][1] = 5.0 + 0.7 * g as f32;
    }
    let mut physics = flat_ground();
    let chunk = Bounds::everywhere();

    let count = flood.fill(&seed(0.5, 0.0, 0.5), &mut physics, &chunk, None);
    assert!(count > 1);
    assert_eq!(flood.hgt_grids()[14][1], 5.0 + 0.7 * 14.0);
    assert_eq!(flood.adj_grids()[15][0].angle(R), 0);
}

#[test]
fn a_conflict_with_no_free_layer_skips_the_move() {
    // the destination's own layer matches, but the stale west link
    // conflicts and every reallocatable layer is taken by far heights:
    // the move is dropped without any allocation
    let mut flood = grid(0.0, 2.0, 1.0);
    flood.hgt_grids_mut()[15][0] = 0.0;
    flood.hgt_grids_mut()[15][1] = 0.0;
    flood.adj_grids_mut()[15][1].set_angle(L, 14);
    for g in 1..MAX_HEIGHTS - 1 {
        flood.hgt_grids_mut()[g][1] = 5.0 + 0.7 * g as f32;
    }
    let mut physics = flat_ground();
    let chunk = Bounds::everywhere();

    let count = flood.fill(&seed(0.5, 0.0, 0.5), &mut physics, &chunk, None);
    assert!(count > 1);
    assert_eq!(flood.adj_grids()[15][0].angle(R), 0);
    // nothing was reallocated or overwritten
    assert_eq!(flood.hgt_grids()[14][1], 5.0 + 0.7 * 14.0);
}

#[test]
#[should_panic(expected = "but adj is")]
fn a_free_claimed_layer_with_live_adjacency_panics() {
    // the best-match scan finds nothing at the destination, so the
    // reference claims the lowest free layer — and kills the process
    // when that layer already carries adjacency
    let mut flood = grid(0.0, 2.0, 1.0);
    flood.hgt_grids_mut()[15][0] = 0.0;
    flood.hgt_grids_mut()[15][1] = 5.0;
    flood.adj_grids_mut()[14][1].set_angle(L, 15);
    let mut physics = flat_ground();
    let chunk = Bounds::everywhere();

    let _ = flood.fill(&seed(0.5, 0.0, 0.5), &mut physics, &chunk, None);
}

#[test]
fn a_matching_opposite_link_reuses_the_cell() {
    // the stale west link points back at the source's own layer: no
    // conflict, but the reciprocal slot blocks a second link, so the
    // pair stays one-way
    let mut flood = grid(0.0, 2.0, 1.0);
    flood.hgt_grids_mut()[15][0] = 0.0;
    flood.hgt_grids_mut()[15][1] = 0.0;
    flood.adj_grids_mut()[15][1].set_angle(L, 15);
    let mut physics = flat_ground();
    let chunk = Bounds::everywhere();

    let count = flood.fill(&seed(0.5, 0.0, 0.5), &mut physics, &chunk, None);
    assert!(count > 1);
    assert_eq!(flood.adj_grids()[15][0].angle(R), 0);
    // the seed was not rolled back: other angles linked fine
    assert_eq!(flood.hgt_grids()[15][0], 0.0);
}

#[test]
fn a_chunk_bound_clip_skips_out_of_space_cells() {
    // the space bounds sit inside the grid: neighbours whose centre
    // falls outside skip at the space probe, so the flood covers just
    // the pocket the bounds admit
    let mut flood = grid(0.0, 2.0, 1.0);
    let mut physics = flat_ground();
    let chunk = Bounds {
        owns_box: (-100.0, -100.0, 100.0, 100.0),
        space: (0.5, 2.5, 0.5, 2.5),
    };

    // the seed plus the four cells whose centres stay inside
    assert_eq!(
        flood.fill(&seed(0.5, 10.0, 0.5), &mut physics, &chunk, None),
        5
    );
    assert_eq!(flood.hgt_grids()[15][0], 0.0);
    assert_eq!(flood.adj_grids()[15][0].angle(UR), 15);
    assert_eq!(flood.hgt_grids()[15][2], -10000.0);
    assert!(flood.boundary_set().is_empty());
}

#[test]
fn check_bounds_retries_after_the_scramble() {
    // ownership only accepts points near the 0.35 mark, so every
    // direct probe fails; the scrambled retry (+0.5) is what lets
    // the flood through
    let mut flood = grid(0.0, 2.0, 1.0);
    let mut physics = flat_ground();
    let chunk = MarkedChunk {
        inner: Bounds::everywhere(),
        mark: 0.35,
    };

    assert_eq!(
        flood.fill(&seed(0.5, 10.0, 0.5), &mut physics, &chunk, None),
        9
    );
    assert!(flood.boundary_set().is_empty());
    assert_eq!(flood.adj_grids()[15][4].all, 0xffffffff);
}

#[test]
fn an_ajar_concave_notch_is_left_alone() {
    // the a=0 concave shape, but with the west cell's north arm
    // missing: the final wildcard clause needs that arm or the south
    // cell's east arm, so the notch survives untouched
    let mut flood = grid(0.0, 2.0, 1.0); // 3x3
    let mut physics = flat_ground();
    let chunk = Bounds::everywhere();
    flood.fill(&seed(0.5, 10.0, 0.5), &mut physics, &chunk, None);
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
    layer[1].set_angle(U, 15); // south cell north
    layer[1].set_angle(UL, 15); // south cell northwest
    layer[1].set_angle(L, 15); // south cell west
    layer[0].set_angle(UR, 15); // keep the graph reciprocal

    flood.postfilter_remove();

    // nothing was removed: the centre keeps all its links
    assert_ne!(flood.adj_grids()[15][4].all, 0);
    assert_eq!(flood.adj_grids()[15][3].angle(R), 15);
    assert_eq!(flood.adj_grids()[15][1].angle(U), 15);
    assert!(flood.check_grid_consistency());
}

#[test]
fn the_lower_regions_pause_until_their_turn() {
    // layers below smallest_height are outside the filter's scan:
    // their cells keep exactly what was written before
    let mut flood = grid(0.0, 4.0, 1.0);
    flood.flash_flood(5.0);
    for k in 0..25usize {
        flood.hgt_grids_mut()[13][k] = k as f32;
    }

    flood.postfilter_remove();

    assert_eq!(flood.hgt_grids()[13][12], 12.0);
    assert_eq!(flood.adj_grids()[13][12].all, 0);
    assert_eq!(flood.adj_grids()[15][12].all, 0xffffffff);
    assert!(flood.boundary_set().is_empty());
}

#[test]
fn the_postfilter_remove_honours_the_boundary_set() {
    // boundary points are exempt from the removal scan
    let mut flood = grid(0.0, 4.0, 1.0);
    let mut physics = flat_ground();
    let chunk = Bounds {
        owns_box: (-100.0, -100.0, 0.5, 100.0),
        space: (-100.0, 100.0, -100.0, 100.0),
    };
    flood.fill(&seed(0.5, 10.0, 2.5), &mut physics, &chunk, None);
    assert!(!flood.boundary_set().is_empty());

    let before = flood.adj_grids().to_vec();
    flood.postfilter_remove();
    assert_eq!(flood.adj_grids(), &before);
}

/// Slab physics whose back-steps against eastward moves alternate
/// between success and failure, standing in for a stateful collision
/// scene.
struct FlakyBack {
    inner: FieldPhysics,
    backs: std::cell::Cell<usize>,
}

impl FloodPhysics for FlakyBack {
    fn get_girth(&self) -> Vector3 {
        self.inner.get_girth()
    }
    fn get_scramble_height(&self) -> f32 {
        self.inner.get_scramble_height()
    }
    fn find_drop_point(&mut self, pos: &Vector3, y: &mut f32) -> bool {
        self.inner.find_drop_point(pos, y)
    }
    fn adjust_move(&mut self, src: &Vector3, dst: &Vector3, dst2: &mut Vector3) {
        self.inner.adjust_move(src, dst, dst2);
        if dst.x < src.x {
            let n = self.backs.get();
            self.backs.set(n + 1);
            if n.is_multiple_of(2) {
                dst2.y = -10000.0;
            }
        }
    }
}

#[test]
fn the_second_back_step_never_fails() {
    // the conflict re-allocation repeats the back-step adjustMove; the
    // reference marks that repetition "Shall NEVER fail here!" — the
    // move is dropped regardless
    let mut flood = grid(0.0, 2.0, 1.0);
    flood.hgt_grids_mut()[15][0] = 0.0;
    flood.hgt_grids_mut()[15][1] = 0.0;
    flood.adj_grids_mut()[15][1].set_angle(L, 14);
    let mut physics = FlakyBack {
        inner: flat_ground(),
        backs: std::cell::Cell::new(0),
    };
    let chunk = Bounds::everywhere();

    // the girth and scramble seams are unused by the flood itself but
    // part of the trait contract
    assert_eq!(physics.get_girth(), Vector3::new(0.5, 1.5, 0.5));
    assert_eq!(physics.get_scramble_height(), 0.5);
    let count = flood.fill(&seed(0.5, 0.0, 0.5), &mut physics, &chunk, None);
    assert!(count > 1);
    // the reallocated layer was never written
    assert_eq!(flood.adj_grids()[15][0].angle(R), 0);
    assert_eq!(flood.hgt_grids()[14][1], -10000.0);
}

#[test]
fn the_seed_cascade_walks_all_four_directions() {
    // the chunk owns a 0.8-wide strip, so no ±res probe pair fits:
    // the seed cascade retries sideways, then forward, then back, and
    // finally reports failure
    let mut flood = grid(0.0, 2.0, 1.0);
    let mut physics = flat_ground();
    let chunk = Bounds {
        owns_box: (-0.4, -100.0, 0.4, 100.0),
        space: (-100.0, 100.0, -100.0, 100.0),
    };
    let mut never = Never;

    // positive seed: +x, then +z, then -x
    assert_eq!(
        flood.fill(
            &seed(3.5, 10.0, 0.5),
            &mut physics,
            &chunk,
            Some(&mut never)
        ),
        0
    );
    // negative seeds: -x and -z arms
    assert_eq!(
        flood.fill(
            &seed(-3.5, 10.0, -1.5),
            &mut physics,
            &chunk,
            Some(&mut never)
        ),
        0
    );
    // a cascade whose first retry lands the probes inside floods the
    // x=1 column of the pocket
    assert_eq!(
        flood.fill(
            &seed(0.5, 10.0, 0.5),
            &mut physics,
            &chunk,
            Some(&mut never)
        ),
        3
    );
    // moves that left the owned strip landed in the boundary set
    assert!(!flood.boundary_set().is_empty());
}

#[test]
#[should_panic(expected = "but adj is")]
fn a_reallocated_layer_with_live_adjacency_panics() {
    // the reference kills the process when a freshly claimed height
    // layer already carries adjacency
    let mut flood = grid(0.0, 2.0, 1.0);
    flood.hgt_grids_mut()[15][0] = 0.0;
    flood.hgt_grids_mut()[15][1] = 0.0;
    flood.adj_grids_mut()[15][1].set_angle(L, 14);
    // layer 14 is free of heights but not of links
    flood.adj_grids_mut()[14][1].set_angle(L, 15);
    let mut physics = flat_ground();
    let chunk = Bounds::everywhere();

    let _ = flood.fill(&seed(0.5, 0.0, 0.5), &mut physics, &chunk, None);
}

#[test]
fn the_wildcards_second_arm_alone_confirms_the_corner() {
    // the concave shape confirmed through the south cell's east arm
    // (the west cell's north arm is missing)
    let mut flood = grid(0.0, 2.0, 1.0); // 3x3
    let mut physics = flat_ground();
    let chunk = Bounds::everywhere();
    flood.fill(&seed(0.5, 10.0, 0.5), &mut physics, &chunk, None);
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
    layer[1].set_angle(U, 15); // south cell north
    layer[1].set_angle(UL, 15); // south cell northwest
    layer[1].set_angle(L, 15); // south cell west
    layer[1].set_angle(R, 15); // south cell east (the confirming arm)
    layer[2].set_angle(L, 15);
    layer[0].set_angle(UR, 15); // keep the southwest link reciprocal

    flood.postfilter_remove();

    assert_eq!(flood.adj_grids()[15][4].all, 0);
    assert_eq!(flood.adj_grids()[15][1].angle(U), 0);
}

#[test]
fn the_corner_removal_clears_the_far_mirror_angles() {
    // the removal also clears the centre's east and north mirrors on
    // their far cells, when those links exist
    let mut flood = grid(0.0, 2.0, 1.0); // 3x3
    let mut physics = flat_ground();
    let chunk = Bounds::everywhere();
    flood.fill(&seed(0.5, 10.0, 0.5), &mut physics, &chunk, None);
    let layer = &mut flood.adj_grids_mut()[15];
    for elt in layer.iter_mut() {
        elt.all = 0;
    }
    layer[4].set_angle(L, 15); // centre west
    layer[4].set_angle(D, 15); // centre south
    layer[4].set_angle(DL, 15); // centre southwest
    layer[4].set_angle(R, 15); // centre east (far mirror east)
    layer[4].set_angle(U, 15); // centre north (far mirror north)
    layer[3].set_angle(R, 15);
    layer[3].set_angle(DR, 15);
    layer[3].set_angle(D, 15);
    layer[3].set_angle(U, 15); // the confirming west arm
    layer[5].set_angle(L, 15);
    layer[7].set_angle(D, 15); // the reciprocal the far mirror clear erases
    layer[1].set_angle(U, 15);
    layer[1].set_angle(UL, 15);
    layer[1].set_angle(L, 15);
    layer[0].set_angle(UR, 15);

    flood.postfilter_remove();

    assert_eq!(flood.adj_grids()[15][4].all, 0);
    // the far mirrors are cleared with the centre
    assert_eq!(flood.adj_grids()[15][5].angle(L), 0);
    assert_eq!(flood.adj_grids()[15][7].angle(D), 0);
}
