//! BW-10-07-04: the chunk obstacle (`lib/chunk/chunk_obstacle.hpp/.cpp`)
//! and its quad tree range computation
//! (`lib/chunk/base_chunk_space.cpp:302-367`).
//!
//! The reference stores `ChunkObstacle*`s in the tree; the port stores
//! arena indices into the owning space's obstacle list, so this type
//! carries the model-space bounding box, the world transform pair and
//! the chunk identifier the pointer comparison (`shutTo_ == pChunk()`)
//! used. The BSP-only face (`ChunkBSPObstacle`) is the one the collide
//! sweep drives.

use std::cell::Cell;
use std::sync::atomic::{AtomicU32, Ordering};

use foundation::math::boundbox::BoundBox;
use foundation::math::matrix::Matrix;
use foundation::math::vector::{Vector2, Vector3};

use physics::{
    BspSweepVisitor, BspTree, CollisionObstacle, CollisionState, QtRange, SpaceCollider,
};

/// `BaseChunkSpace::s_obstacleTreeDepth` (`base_chunk_space.cpp:52`) —
/// the shipped depth the column obstacle trees are built with (the
/// reference setter `obstacleTreeDepth(int)` has no port-side writer —
/// registered as a constant).
pub const OBSTACLE_TREE_DEPTH: usize = 5;

/// `ChunkObstacle::s_nextMark_` (`chunk_obstacle.cpp:31`).
static NEXT_MARK: AtomicU32 = AtomicU32::new(0);

/// `ChunkObstacle::nextMark` (`chunk_obstacle.hpp:63`): the space's
/// collide walk bumps it once per sweep.
pub(crate) fn next_mark() -> u32 {
    NEXT_MARK.fetch_add(1, Ordering::Relaxed);
    NEXT_MARK.load(Ordering::Relaxed)
}

/// The current `s_nextMark_` value (read, no bump) — what a fresh
/// obstacle's mark is seeded against.
fn peek_mark() -> u32 {
    NEXT_MARK.load(Ordering::Relaxed)
}

/// `ChunkObstacle` (`chunk_obstacle.hpp:39-93`): the BSP-backed slice.
/// The reference refcounts through `heldObstacles_`; the port owns
/// obstacles by value in the space arena (a clone bump — no port-side
/// sharing face exists).
#[derive(Debug, Clone)]
pub struct ChunkObstacle {
    /// `mark_` — the per-sweep dedup stamp.
    mark: Cell<u32>,
    /// `bb_` — the model-space bounding box (`calculateQTRange` and the
    /// sweep's local clip both apply the transform pair themselves).
    pub bb: BoundBox,
    /// `pItem_->chunk()` — the owning chunk identifier; `None` mirrors a
    /// null `pChunk()` (the traverse skips those).
    chunk: Option<String>,
    /// `transform_`.
    pub transform: Matrix,
    /// `transformInverse_`.
    pub transform_inverse: Matrix,
    /// `bspTree_` — the `ChunkBSPObstacle` model.
    pub bsp: BspTree,
}

impl ChunkObstacle {
    /// `ChunkObstacle::ChunkObstacle` (`chunk_obstacle.cpp:42-53`): the
    /// mark starts 16 generations behind the current one (the reference
    /// pads the gap so stale marks never alias the next sweep's stamp).
    pub fn new(bsp: BspTree, transform: Matrix, bb: BoundBox, chunk: Option<String>) -> Self {
        let mut transform_inverse = transform;
        transform_inverse.invert();
        Self {
            mark: Cell::new(peek_mark().wrapping_sub(16)),
            bb,
            chunk,
            transform,
            transform_inverse,
            bsp,
        }
    }

    /// `ChunkObstacle::mark` (`chunk_obstacle.cpp:46-52`): true when the
    /// obstacle was already visited this sweep; otherwise it stamps
    /// itself with the current mark.
    pub fn mark(&self) -> bool {
        let current = peek_mark();
        if self.mark.get() == current {
            return true;
        }
        self.mark.set(current);
        false
    }

    /// `pChunk()`.
    pub fn chunk(&self) -> Option<&str> {
        self.chunk.as_deref()
    }
}

/// `calculateQTRange` (`base_chunk_space.cpp:302-360`): the world-space
/// bounding box projected onto the tree's grid; C++ `int` truncation and
/// the Rust `as i32` cast agree for in-range values (the large-object
/// scaling is `#if 0` in the reference and not ported).
pub fn calculate_qt_range(
    input: &ChunkObstacle,
    origin: Vector2,
    range: f32,
    depth: usize,
) -> QtRange {
    let width = (1usize << depth) as f32;

    let mut bb = input.bb;
    bb.transform_by(&input.transform);

    QtRange {
        left: (width * (bb.min.x - origin.x) / range) as i32,
        right: (width * (bb.max.x - origin.x) / range) as i32,
        bottom: (width * (bb.min.z - origin.y) / range) as i32,
        top: (width * (bb.max.z - origin.y) / range) as i32,
    }
}

impl SpaceCollider for ChunkObstacle {
    /// `ChunkBSPObstacle::collide` (`chunk_obstacle.cpp:152-176`): the
    /// BSP sweep with the 1.0-seeded ratio. The moving-obstacle flag
    /// rides the reference's `SceneObject` payload (`DYNAMIC_OBSTACLE_
    /// USER_FLAG`), which the `CollisionObstacle` port does not model —
    /// registered.
    fn collide_point(&self, source: Vector3, extent: Vector3, state: &mut CollisionState) -> bool {
        let ob = CollisionObstacle::new(&self.transform, &self.transform_inverse);
        let mut visitor = BspSweepVisitor::new(ob, state);
        let mut rd = 1.0f32;
        self.bsp
            .intersects(source, extent, &mut rd, Some(&mut visitor));
        visitor.should_stop()
    }
}

/// Fixture blobs shared by the crate's obstacle-facing unit tests — the
/// binary face of a reference-built one-triangle BSP (the port builds no
/// trees from triangles; `BSPTreeTool::buildBSP` is the reference-side
/// producer).
#[cfg(test)]
pub(crate) mod test_bsp {
    use foundation::math::boundbox::BoundBox;
    use foundation::math::matrix::Matrix;
    use foundation::math::vector::Vector3;
    use physics::{BSP_FILE_CUR_TOKEN, BSP_NODE_PACKED_SIZE, BspTree, OFFSET_32BIT_NULL};

    use super::ChunkObstacle;

    pub fn v(x: f32, y: f32, z: f32) -> Vector3 {
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

    fn node_bytes(front: i32, back: i32, normal: Vector3, d: f32, num_tris: u32) -> Vec<u8> {
        let mut b = Vec::new();
        b.extend_from_slice(&front.to_le_bytes());
        b.extend_from_slice(&back.to_le_bytes());
        b.extend(&v3_bytes(normal));
        b.extend_from_slice(&d.to_le_bytes());
        b.extend_from_slice(&0i32.to_le_bytes());
        b.extend_from_slice(&num_tris.to_le_bytes());
        b.extend_from_slice(&OFFSET_32BIT_NULL.to_le_bytes());
        b.extend_from_slice(&0u32.to_le_bytes());
        assert_eq!(b.len(), BSP_NODE_PACKED_SIZE as usize);
        b
    }

    /// A one-triangle BSP facing +z at z = 5, box (0,0,0)-(10,10,5) —
    /// the reference-built fixture blob's binary face.
    pub fn flat_bsp() -> BspTree {
        let mut b = Vec::new();
        b.extend(&words(&[BSP_FILE_CUR_TOKEN, 16, 40, 40, 1, 1, 0]));
        b.extend(&v3_bytes(v(0.0, 0.0, 0.0)));
        b.extend(&v3_bytes(v(10.0, 10.0, 5.0)));
        b.extend(&[0u8; 8]);
        b.extend(&tri_bytes(
            v(0.0, 0.0, 5.0),
            v(10.0, 0.0, 5.0),
            v(0.0, 10.0, 5.0),
        ));
        b.extend(&node_bytes(
            OFFSET_32BIT_NULL,
            OFFSET_32BIT_NULL,
            v(0.0, 0.0, 1.0),
            5.0,
            1,
        ));
        BspTree::load(&b).expect("the fixture blob loads")
    }

    /// The slab obstacle at the identity transform, owned by `chunk`.
    pub fn slab_obstacle(chunk: Option<&str>) -> ChunkObstacle {
        ChunkObstacle::new(
            flat_bsp(),
            Matrix::identity(),
            BoundBox::new(v(0.0, 0.0, 0.0), v(10.0, 10.0, 5.0)),
            chunk.map(|c| c.to_string()),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::test_bsp::{flat_bsp, slab_obstacle, v};
    use super::*;
    use physics::{ClosestTriangle, SweepParams, SweepShape, collide_into_object_point};

    #[test]
    fn the_ctor_seeds_the_mark_behind_the_current_stamp() {
        let obstacle = slab_obstacle(Some("001x000x"));

        // a fresh mark is 16 behind: the first visit is not a repeat
        assert!(!obstacle.mark());
        // the second visit in the same sweep is
        assert!(obstacle.mark());
    }

    #[test]
    fn the_next_mark_rotates_the_dedup_stamp() {
        let obstacle = slab_obstacle(Some("001x000x"));
        assert!(!obstacle.mark());

        // the next sweep bumps the stamp, so the stale mark misses
        next_mark();
        assert!(!obstacle.mark());
        assert!(obstacle.mark());

        // a fresh obstacle seeds from the bumped stamp
        let fresh = slab_obstacle(Some("001x000x"));
        assert!(!fresh.mark());
    }

    #[test]
    fn the_null_chunk_mirror_reports_no_owner() {
        assert_eq!(slab_obstacle(Some("001x000x")).chunk(), Some("001x000x"));
        assert_eq!(slab_obstacle(None).chunk(), None);
    }

    #[test]
    fn the_qt_range_projects_the_world_box() {
        // identity transform: the model box is the world box. depth 5
        // => WIDTH 32, range 100.
        let origin = Vector2::new(0.0, 0.0);
        let obstacle = slab_obstacle(Some("001x000x"));
        let range = calculate_qt_range(&obstacle, origin, 100.0, 5);
        assert_eq!(
            (range.left, range.bottom, range.right, range.top),
            (0, 0, 3, 1)
        );

        // +50/+25 in world: the world box becomes (50,25)-(60,35) in
        // x/z, so the cells shift by 16/8
        let mut transform = Matrix::identity();
        transform.set_translate(50.0, 0.0, 25.0);
        let moved = ChunkObstacle::new(
            flat_bsp(),
            transform,
            BoundBox::new(v(0.0, 0.0, 0.0), v(10.0, 10.0, 5.0)),
            Some("001x000x".to_string()),
        );
        let range = calculate_qt_range(&moved, origin, 100.0, 5);
        assert_eq!(
            (range.left, range.bottom, range.right, range.top),
            (16, 8, 19, 9)
        );

        // a negative world box truncates toward zero like the C++ cast
        // (-12.8 -> -12, not -13)
        let mut transform = Matrix::identity();
        transform.set_translate(-50.0, 0.0, -25.0);
        let moved = ChunkObstacle::new(
            flat_bsp(),
            transform,
            BoundBox::new(v(0.0, 0.0, 0.0), v(10.0, 10.0, 5.0)),
            Some("001x000x".to_string()),
        );
        let range = calculate_qt_range(&moved, origin, 100.0, 5);
        assert_eq!(
            (range.left, range.bottom, range.right, range.top),
            (-16, -8, -12, -6)
        );
    }

    #[test]
    fn the_space_collider_hits_through_the_bsp() {
        let obstacle = slab_obstacle(Some("001x000x"));
        let sp = SweepParams::new(&SweepShape::Ray(v(5.0, 5.0, 0.0)), v(5.0, 5.0, 10.0));
        let mut fdpt = ClosestTriangle::new();
        let (hit, dist) = {
            let mut cs = CollisionState::new(&mut fdpt);
            let hit = collide_into_object_point(
                &sp,
                &obstacle,
                &obstacle.bb,
                &obstacle.transform_inverse,
                &mut cs,
            );
            (hit, cs.dist)
        };
        // ClosestTriangle answers COLLIDE_BEFORE — the sweep never stops
        // (the bool stays false) but the state keeps the nearest dist,
        // which is what `ChunkSpace::collide` returns to findDropPoint.
        assert!(!hit);
        assert!((dist - 5.0).abs() < 1e-5, "dist {dist}");
        // the recorded triangle is the hit plane at z 5
        assert!((fdpt.triangle().v0().z - 5.0).abs() < 1e-5);

        // a ray outside the slab's x span misses on the bb clip: the
        // state and the recorder stay untouched
        let sp = SweepParams::new(&SweepShape::Ray(v(15.0, 5.0, 0.0)), v(15.0, 5.0, 10.0));
        let mut fdpt = ClosestTriangle::new();
        let (hit, dist) = {
            let mut cs = CollisionState::new(&mut fdpt);
            let hit = collide_into_object_point(
                &sp,
                &obstacle,
                &obstacle.bb,
                &obstacle.transform_inverse,
                &mut cs,
            );
            (hit, cs.dist)
        };
        assert!(!hit);
        assert_eq!(dist, -1.0);
        assert_eq!(fdpt.triangle().v0(), Vector3::ZERO);
    }
}
