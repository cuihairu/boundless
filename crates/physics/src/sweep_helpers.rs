//! BW-10-07-04: `SweepHelpers` — the BSP sweep visitor
//! (`lib/physics2/sweep_helpers.cpp:17-41`) and the space-side
//! `collideIntoObject` ray instantiation (`sweep_helpers.hpp:65-103`).
//!
//! `portal.rs` carries the twin of `collideIntoObject` for portal
//! obstacles, threading the owning `Portal` through its
//! `CollisionInterface`. Chunk-space obstacles have no portal in hand, so
//! this face takes a [`SpaceCollider`] instead; everything between the
//! world→local transform and the collide dispatch is the same reference
//! code. The extruded-prism instantiation (`SweepShape<WorldTriangle>`)
//! has no port-side space face (registered) — the constant bloat here is
//! the ray shape's `transformRangeToRadius() + 0.01f`.

use foundation::math::boundbox::BoundBox;
use foundation::math::matrix::Matrix;
use foundation::math::vector::Vector3;

use crate::bsp::CollisionVisitor;
use crate::portal::{CollisionObstacle, CollisionState, SweepParams};
use crate::world_triangle::WorldTriangle;

/// `SweepHelpers::BSPSweepVisitor`: the adapter a BSP tree's sweep calls
/// back into. It turns each local hit triangle into a
/// `CollisionState::cc` visit with the hit distance remapped onto the
/// full sweep, and folds the callback's say bits into the sweep windows.
pub struct BspSweepVisitor<'s, 'c> {
    /// `ob_` — the obstacle handle handed to the callback.
    obstacle: CollisionObstacle,
    /// `cs_` — the shared sweep state.
    state: &'s mut CollisionState<'c>,
    /// `stop_` — the callback asked to stop.
    stop: bool,
}

impl<'s, 'c> BspSweepVisitor<'s, 'c> {
    pub fn new(obstacle: CollisionObstacle, state: &'s mut CollisionState<'c>) -> Self {
        Self {
            obstacle,
            state,
            stop: false,
        }
    }

    /// `shouldStop`.
    pub fn should_stop(&self) -> bool {
        self.stop
    }
}

impl CollisionVisitor for BspSweepVisitor<'_, '_> {
    /// `BSPSweepVisitor::visit` (`sweep_helpers.cpp:26-41`).
    fn visit(&mut self, triangle: &WorldTriangle, rd: f32) -> bool {
        let ndist = self.state.s_travel + (self.state.e_travel - self.state.s_travel) * rd;

        if self.state.only_less && ndist > self.state.dist {
            return true;
        }
        if self.state.only_more && ndist < self.state.dist {
            return false;
        }

        let say = self.state.cc.visit(&self.obstacle, triangle, ndist);

        if (say & 3) != 3 {
            self.state.dist = ndist;
        }

        if say == 0 {
            self.stop = true;
            return true;
        }

        self.state.only_less = (say & 2) == 0;
        self.state.only_more = (say & 1) == 0;

        self.state.only_less
    }
}

/// The point overload of `CollisionInterface`
/// (`collision_interface.hpp`) as the chunk-space obstacles implement it —
/// the twin for portal obstacles (`PortalObstacle`) lives in `portal.rs`
/// with the owning portal threaded through.
pub trait SpaceCollider {
    /// `collide(source, extent, state)` — the extruded point overload.
    fn collide_point(&self, source: Vector3, extent: Vector3, state: &mut CollisionState) -> bool;
}

/// `SweepHelpers::collideIntoObject`, the ray instantiation
/// (`sweep_helpers.hpp:65-103`): the sweep's centre line goes into
/// obstacle-local coordinates, the biggest axis of the difference picks
/// the travel axis, the bb clip (with the 1cm pad) rejects or narrows the
/// window, the travel parameters remap to the sweep's full distance and
/// the narrowed ray goes into the obstacle's collide.
pub fn collide_into_object_point(
    sp: &SweepParams,
    collision_funcs: &dyn SpaceCollider,
    obstacle_local_bb: &BoundBox,
    obstacle_inv_transform: &Matrix,
    cs: &mut CollisionState,
) -> bool {
    let s_tr = obstacle_inv_transform.apply_point(&sp.csource());
    let e_tr = obstacle_inv_transform.apply_point(&sp.cextent());

    let bax = crate::portal::biggest_axis_of_difference(&e_tr, &s_tr);

    let s_trba = s_tr.get(bax);
    let d_trba = e_tr.get(bax) - s_trba;

    // Clip the line to the bounding box ('tho it should always
    //  be inside since we found it through the hull tree). The ray
    // shape's transformRangeToRadius is 0, so the bloat is the 1cm pad.
    let bloat = 0.01;

    let mut s_tr_clipped = s_tr;
    let mut e_tr_clipped = e_tr;
    if !obstacle_local_bb.clip(&mut s_tr_clipped, &mut e_tr_clipped, bloat) {
        return false;
    }

    // set traveled and traveled to be the start and end distances
    //  along the line (not their original use, but it fits)
    cs.s_travel = (s_tr_clipped.get(bax) - s_trba) / d_trba * sp.full_distance();
    cs.e_travel = (e_tr_clipped.get(bax) - s_trba) / d_trba * sp.full_distance();

    // see if we can reject this bb outright
    if cs.only_less && cs.s_travel > cs.dist {
        return false;
    }

    if cs.only_more && cs.e_travel < cs.dist {
        return false;
    }

    // ok, let's search in it then — a ray's swept shape is the clipped
    // centre line itself (`SweepShape<Vector3>::transform`).
    collision_funcs.collide_point(s_tr_clipped, e_tr_clipped, cs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bsp::{BSP_FILE_CUR_TOKEN, BSP_NODE_PACKED_SIZE, BspTree, OFFSET_32BIT_NULL};
    use crate::portal::{COLLIDE_ALL, COLLIDE_BEFORE, COLLIDE_STOP, CollisionCallback, SweepShape};
    use foundation::math::vector::Vector3;

    fn v(x: f32, y: f32, z: f32) -> Vector3 {
        Vector3::new(x, y, z)
    }

    fn words(vals: &[u32]) -> Vec<u8> {
        vals.iter().flat_map(|w| w.to_le_bytes()).collect()
    }

    fn v3_bytes(v: Vector3) -> Vec<u8> {
        let mut b = Vec::new();
        b.extend_from_slice(&v.x.to_le_bytes());
        b.extend_from_slice(&v.y.to_le_bytes());
        b.extend_from_slice(&v.z.to_le_bytes());
        b
    }

    fn tri_bytes(v0: Vector3, v1: Vector3, v2: Vector3) -> Vec<u8> {
        let mut b = Vec::new();
        b.extend(&v3_bytes(v0));
        b.extend(&v3_bytes(v1));
        b.extend(&v3_bytes(v2));
        b.extend(&0u16.to_le_bytes());
        b.extend(&[0u8; 2]);
        b
    }

    fn node_bytes(
        front: i32,
        back: i32,
        normal: Vector3,
        d: f32,
        tris: i32,
        num_tris: u32,
    ) -> Vec<u8> {
        let mut b = Vec::new();
        b.extend_from_slice(&front.to_le_bytes());
        b.extend_from_slice(&back.to_le_bytes());
        b.extend(&v3_bytes(normal));
        b.extend_from_slice(&d.to_le_bytes());
        b.extend_from_slice(&tris.to_le_bytes());
        b.extend_from_slice(&num_tris.to_le_bytes());
        b.extend_from_slice(&OFFSET_32BIT_NULL.to_le_bytes());
        b.extend_from_slice(&0u32.to_le_bytes());
        assert_eq!(b.len(), BSP_NODE_PACKED_SIZE as usize);
        b
    }

    /// A one-triangle BSP facing +z at z = 5, box (0,0,0)-(10,10,5): the
    /// binary face of the reference-built fixture blobs (the port builds
    /// no trees from triangles — `BSPTreeTool::buildBSP` is the
    /// reference-side fixture producer).
    fn flat_bsp() -> BspTree {
        let tri = (v(0.0, 0.0, 5.0), v(10.0, 0.0, 5.0), v(0.0, 10.0, 5.0));
        let mut b = Vec::new();
        b.extend(&words(&[BSP_FILE_CUR_TOKEN, 16, 40, 40, 1, 1, 0]));
        b.extend(&v3_bytes(v(0.0, 0.0, 0.0)));
        b.extend(&v3_bytes(v(10.0, 10.0, 5.0)));
        b.extend(&[0u8; 8]);
        b.extend(&tri_bytes(tri.0, tri.1, tri.2));
        b.extend(&node_bytes(
            OFFSET_32BIT_NULL,
            OFFSET_32BIT_NULL,
            v(0.0, 0.0, 1.0),
            5.0,
            0,
            1,
        ));
        BspTree::load(&b).expect("the fixture blob loads")
    }

    /// A callback that records visits and answers a fixed say value (the
    /// modern hook, so the water fallback never fires).
    struct Recorder {
        say: i32,
        seen: Vec<(Vector3, u16, f32)>,
    }

    impl Recorder {
        fn new(say: i32) -> Self {
            Self {
                say,
                seen: Vec::new(),
            }
        }
    }

    impl CollisionCallback for Recorder {
        fn do_visit(
            &mut self,
            obstacle: &CollisionObstacle,
            triangle: &WorldTriangle,
            dist: f32,
        ) -> i32 {
            self.seen
                .push((obstacle.transform.apply_to_origin(), triangle.flags(), dist));
            self.say
        }
    }

    /// `ChunkBSPObstacle::collide` over the flat fixture tree, at a
    /// caller-supplied world transform (`chunk_obstacle.cpp:152-176`).
    struct FlatObstacle {
        bsp: BspTree,
        transform: Matrix,
    }

    impl FlatObstacle {
        fn at(transform: Matrix) -> Self {
            Self {
                bsp: flat_bsp(),
                transform,
            }
        }

        fn inverse(&self) -> Matrix {
            let mut inverse = self.transform;
            inverse.invert();
            inverse
        }

        fn bb(&self) -> BoundBox {
            BoundBox::new(v(0.0, 0.0, 0.0), v(10.0, 10.0, 5.0))
        }
    }

    impl SpaceCollider for FlatObstacle {
        fn collide_point(
            &self,
            source: Vector3,
            extent: Vector3,
            state: &mut CollisionState,
        ) -> bool {
            let mut transform_inverse = self.transform;
            transform_inverse.invert();
            let ob = CollisionObstacle::new(&self.transform, &transform_inverse);
            let mut visitor = BspSweepVisitor::new(ob, state);
            let mut rd = 1.0f32;
            self.bsp
                .intersects(source, extent, &mut rd, Some(&mut visitor));
            visitor.should_stop()
        }
    }

    fn ray(start: Vector3, end: Vector3) -> SweepParams {
        SweepParams::new(&SweepShape::Ray(start), end)
    }

    #[test]
    fn the_visitor_remaps_the_hit_onto_the_full_sweep() {
        let obstacle = FlatObstacle::at(Matrix::identity());
        let sp = ray(v(5.0, 5.0, 0.0), v(5.0, 5.0, 10.0));
        let mut cb = Recorder::new(COLLIDE_STOP);
        let (hit, dist_after) = {
            let mut cs = CollisionState::new(&mut cb);
            let hit = collide_into_object_point(
                &sp,
                &obstacle,
                &obstacle.bb(),
                &obstacle.inverse(),
                &mut cs,
            );
            (hit, cs.dist)
        };
        let seen = cb.seen.clone();
        assert!(hit, "the callback stopped on the first hit");
        assert_eq!(seen.len(), 1);
        // the obstacle origin rode through
        assert_eq!(seen[0].0, Vector3::ZERO);
        // sTravel = eTravel = 5 (the box spans the whole sweep in z), the
        // hit sits at the plane rd 0.5 -> ndist 5
        assert!((seen[0].2 - 5.0).abs() < 1e-4, "dist {}", seen[0].2);
        assert_eq!(dist_after, seen[0].2);
    }

    #[test]
    fn the_bb_clip_rejects_a_miss_outside_the_box() {
        let obstacle = FlatObstacle::at(Matrix::identity());
        let sp = ray(v(15.0, 5.0, 0.0), v(15.0, 5.0, 10.0));
        let mut cb = Recorder::new(COLLIDE_STOP);
        let (hit, s_travel) = {
            let mut cs = CollisionState::new(&mut cb);
            let hit = collide_into_object_point(
                &sp,
                &obstacle,
                &obstacle.bb(),
                &obstacle.inverse(),
                &mut cs,
            );
            (hit, cs.s_travel)
        };
        let visits = cb.seen.len();
        assert!(!hit);
        assert_eq!(visits, 0);
        // the rejection leaves the state untouched
        assert_eq!(s_travel, 0.0);
    }

    #[test]
    fn only_more_rejects_hits_before_the_window() {
        let obstacle = FlatObstacle::at(Matrix::identity());
        let sp = ray(v(5.0, 5.0, 0.0), v(5.0, 5.0, 10.0));
        let mut cb = Recorder::new(COLLIDE_ALL);
        let hit = {
            let mut cs = CollisionState::new(&mut cb);
            cs.only_more = true;
            cs.dist = f32::MAX;
            collide_into_object_point(&sp, &obstacle, &obstacle.bb(), &obstacle.inverse(), &mut cs)
        };
        // ndist 5 < f32::MAX: the eTravel window rejects the whole obstacle
        assert!(!hit);
        assert_eq!(cb.seen.len(), 0);
    }

    #[test]
    fn the_say_bits_drive_the_visitor_windows() {
        let obstacle = FlatObstacle::at(Matrix::identity());
        let sp = ray(v(5.0, 5.0, 0.0), v(5.0, 5.0, 10.0));

        // COLLIDE_BEFORE (1): keeps sweeping, onlyLess set at the end
        let mut cb = Recorder::new(COLLIDE_BEFORE);
        let (hit, only_less) = {
            let mut cs = CollisionState::new(&mut cb);
            let hit = collide_into_object_point(
                &sp,
                &obstacle,
                &obstacle.bb(),
                &obstacle.inverse(),
                &mut cs,
            );
            (hit, cs.only_less)
        };
        assert!(!hit);
        assert_eq!(cb.seen.len(), 1);
        assert!(only_less);

        // COLLIDE_ALL (3): (say & 3) == 3, so dist stays untouched
        let mut cb = Recorder::new(COLLIDE_ALL);
        let (hit, dist_after) = {
            let mut cs = CollisionState::new(&mut cb);
            let hit = collide_into_object_point(
                &sp,
                &obstacle,
                &obstacle.bb(),
                &obstacle.inverse(),
                &mut cs,
            );
            (hit, cs.dist)
        };
        assert!(!hit);
        assert_eq!(dist_after, -1.0);
    }

    #[test]
    fn a_transformed_obstacle_lifts_through_its_inverse() {
        // the obstacle sits +10 in world x: a world ray at x=15 reaches
        // the local box (0,0,0)-(10,10,5) through the inverse transform
        let mut transform = Matrix::identity();
        transform.set_translate(10.0, 0.0, 0.0);
        let obstacle = FlatObstacle::at(transform);

        let sp = ray(v(15.0, 5.0, 0.0), v(15.0, 5.0, 10.0));
        let mut cb = Recorder::new(COLLIDE_STOP);
        let hit = {
            let mut cs = CollisionState::new(&mut cb);
            collide_into_object_point(&sp, &obstacle, &obstacle.bb(), &obstacle.inverse(), &mut cs)
        };
        let seen = cb.seen.clone();
        assert!(hit, "the world ray lands in the shifted local box");
        assert_eq!(seen.len(), 1);
        // the visit's triangle is the local one; the callback's obstacle
        // origin is the world transform
        assert_eq!(seen[0].0, v(10.0, 0.0, 0.0));
    }
}
