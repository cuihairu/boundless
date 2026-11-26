//! BW-10-01-05 the space index columns, a port of `BaseChunkSpace::Column`
//! and the server `ColumnGrid` (`base_chunk_space.cpp`, `server_chunk_space`).
//!
//! A column is one grid cell's slice of the space: which chunks reach into
//! it. Outside chunks sit in `outside_chunk` (one per column); inside chunks
//! go into the column's `HullTree` keyed by their hull border. `focus` fills
//! the columns, `find_chunk*` queries them, `open_and_see`/`shut_if_seen`
//! drive the focus bookkeeping.
//!
//! The reference stores raw `Chunk*` pointers (`HullChunk` wrappers); the
//! port stores snapshots (`ColumnChunk`) because the chunk map lives behind
//! the space's borrow. Not ported (registered): the obstacle tree and
//! `jogForeignItems` (scene-adaptation surface, BW-10-02), the `stale` flag
//! and `size`/`dumpDebug` diagnostics.

use std::collections::BTreeMap;

use foundation::math::{PlaneEq, Vector3};
use physics::QuadTree;

use crate::chunk::Chunk;
use crate::hulltree::HullTree;
use crate::obstacle::{ChunkObstacle, OBSTACLE_TREE_DEPTH, calculate_qt_range};

/// `BaseChunkSpace::Column` (`base_chunk_space.cpp:377-391`): the hull
/// tree of the inside chunks overlapping it, the owning outside chunk,
/// and the obstacle tree the collide sweep walks.
#[derive(Debug, Clone)]
pub struct Column {
    /// `pChunkTree_`: the inside chunks' hulls.
    chunk_tree: Option<HullTree>,
    /// `pOutsideChunk_`.
    outside_chunk: Option<ColumnChunk>,
    /// `holdings_`: every chunk ever added, wrapped originals included.
    holdings: Vec<ColumnChunk>,
    /// `shutTo_`: the chunk this column is currently shut to.
    shut_to: Option<String>,
    /// `pObstacleTree_` — `new ObstacleTree(pSpace->gridToPoint(x),
    /// pSpace->gridToPoint(z), obstacleTreeDepth(), pSpace->gridSize())`,
    /// with `gridToPoint = grid * gridSize`.
    obstacle_tree: QuadTree,
}

impl Column {
    /// `BaseChunkSpace::Column::Column` (`base_chunk_space.cpp:377-387`):
    /// the obstacle tree is allocated eagerly (the reference ctor always
    /// makes one); the depth is the shipped 5.
    pub fn new(grid_x: i32, grid_z: i32, grid_size: f32) -> Self {
        Self {
            tree: None,
            outside: None,
            holdings: Vec::new(),
            seen: Vec::new(),
            shut_to: None,
            obstacle_tree: QuadTree::new(
                grid_x as f32 * grid_size,
                grid_z as f32 * grid_size,
                OBSTACLE_TREE_DEPTH,
                grid_size,
            ),
        }
    }

    /// `Column::obstacles` (`base_chunk_space.hpp:142`).
    pub fn obstacles(&self) -> &QuadTree {
        &self.obstacle_tree
    }

    /// `BaseChunkSpace::Column::addObstacle` (`base_chunk_space.cpp`):
    /// a column shut to a chunk refuses that chunk's obstacles. The
    /// reference refcounts through `heldObstacles_` — the port's obstacle
    /// arena in the owning space owns the lifetime instead (registered);
    /// the tree stores the arena index and computes the range off the
    /// obstacle, exactly like the reference's tree add does.
    pub fn add_obstacle(&mut self, obstacle: &ChunkObstacle, index: u32) {
        if self.shut_to.as_deref() == obstacle.chunk() {
            return;
        }

        let range = calculate_qt_range(
            obstacle,
            self.obstacle_tree.origin(),
            self.obstacle_tree.range(),
            self.obstacle_tree.depth(),
        );
        self.obstacle_tree.add(index, range);
    }

    /// `Column::addChunk` (`base_chunk_space.cpp:423-464`): take a holding in
    /// the chunk; outside chunks replace `pOutsideChunk_` (keeping whichever
    /// has **more** static items — a tie keeps the old one), inside chunks
    /// add their hull to the tree.
    pub fn add_chunk(&mut self, border: &HullBorder, chunk: ColumnChunk) {
        // See if this column has already seen this chunk.
        if self.shut_to.as_deref() == Some(&chunk.identifier) {
            return;
        }

        // The holding wraps the *original* chunk: it is created before any
        // outside-chunk reassignment, exactly like the reference's
        // `new HullChunk(pChunk)` (`base_chunk_space.cpp:429`).
        let holding_idx = self.holdings.len();
        self.holdings.push(chunk.clone());

        if chunk.is_outside {
            if let Some(existing) = &self.outside_chunk {
                // Take whichever has the most static chunk items; a tie
                // keeps the incumbent. The reference ERRORs here first.
                if chunk.static_items > existing.static_items {
                    self.outside_chunk = Some(chunk);
                }
            } else {
                self.outside_chunk = Some(chunk);
            }
        } else {
            let tree = self.chunk_tree.get_or_insert_with(HullTree::new);
            tree.add(border, holding_idx);
            // The reference shakes up the outside chunk's foreign items here
            // (`jogForeignItems`); that is scene-adaptation surface (BW-10-02).
        }
    }

    /// `hasInsideChunks` (`base_chunk_space.hpp:135`).
    pub fn has_inside_chunks(&self) -> bool {
        self.chunk_tree.is_some()
    }

    /// `Column::findChunk` (`base_chunk_space.cpp:570-593`): start at the
    /// outside chunk; the first hull candidate replaces it unconditionally
    /// (`pChunk == pOutsideChunk_` holds while nothing was adopted, and
    /// `NULL == NULL` when there is no outside chunk), and after that a
    /// candidate replaces the incumbent only while the incumbent is
    /// strictly bigger (`pChunk->volume() > result->pChunk_->volume()`) —
    /// the walk converges on the **smallest** hull containing the point,
    /// the innermost room of nested shells.
    pub fn find_chunk(&self, point: &Vector3) -> Option<&ColumnChunk> {
        let mut current = self.outside_chunk.as_ref();
        if let Some(tree) = &self.chunk_tree {
            for idx in tree.test_point(point) {
                let result = &self.holdings[idx];
                let same_outside = match (current, &self.outside_chunk) {
                    (None, None) => true,
                    (Some(c), Some(o)) => c.identifier == o.identifier,
                    _ => false,
                };
                if same_outside || current.is_some_and(|c| c.volume > result.volume) {
                    current = Some(result);
                }
            }
        }
        current
    }

    /// `Column::findChunkExcluding` (`base_chunk_space.cpp:539-564`): the
    /// same walk, ignoring one chunk and requiring the candidate's bounding
    /// box to contain the point.
    pub fn find_chunk_excluding(&self, point: &Vector3, not: &str) -> Option<&ColumnChunk> {
        let mut current = match self.outside_chunk.as_ref() {
            Some(o) if o.identifier != not => Some(o),
            _ => None,
        };
        if let Some(tree) = &self.chunk_tree {
            for idx in tree.test_point(point) {
                let result = &self.holdings[idx];
                let same_outside = match (current, &self.outside_chunk) {
                    (None, None) => true,
                    (Some(c), Some(o)) => c.identifier == o.identifier,
                    _ => false,
                };
                let adopt = (current.is_none()
                    || same_outside
                    || current.is_some_and(|c| c.volume > result.volume))
                    && result.bounding_box.contains(point);
                if adopt && result.identifier != not {
                    current = Some(result);
                }
            }
        }
        current
    }

    /// `pOutsideChunk()` (`base_chunk_space.hpp:148`).
    pub fn outside_chunk(&self) -> Option<&ColumnChunk> {
        self.outside_chunk.as_ref()
    }

    /// `Column::openAndSee` (`base_chunk_space.cpp:626-635`).
    pub fn open_and_see(&mut self, identifier: &str) {
        // Only add it if we weren't already shut to it; if we were shut to
        // it then we must have seen it already.
        if self.shut_to.as_deref() != Some(identifier) {
            self.seen.push(identifier.to_string());
        }
        self.shut_to = None;
    }

    /// `Column::shutIfSeen` (`base_chunk_space.cpp:641-652`).
    pub fn shut_if_seen(&mut self, identifier: &str) {
        self.shut_to = if self.seen.iter().any(|s| s == identifier) {
            Some(identifier.to_string())
        } else {
            None
        };
    }

    /// The identifiers of every holding, in add order (test/inspection face
    /// for what the reference's `holdings_` list carries).
    pub fn holdings_identifiers(&self) -> Vec<&str> {
        self.holdings
            .iter()
            .map(|h| h.identifier.as_str())
            .collect()
    }

    /// How many times `identifier` was added (the per-column focus holding
    /// count the test face reads).
    pub fn holdings_of(&self, identifier: &str) -> usize {
        self.holdings
            .iter()
            .filter(|h| h.identifier == identifier)
            .count()
    }

    pub fn is_shut_to(&self, identifier: &str) -> bool {
        self.shut_to.as_deref() == Some(identifier)
    }

    pub fn has_seen(&self, identifier: &str) -> bool {
        self.seen.iter().any(|s| s == identifier)
    }

    /// How many open records the column carries (`seen_.size()`).
    pub fn seen_len(&self) -> usize {
        self.seen.len()
    }
}

/// The server's focus grid (`server_chunk_space.hpp:26-73`): a rectangle of
/// optional columns over the mapping's world bounds.
#[derive(Debug, Clone, Default)]
pub struct ColumnGrid {
    grid: Vec<Option<Column>>,
    x_origin: i32,
    z_origin: i32,
    x_size: usize,
    z_size: usize,
}

impl ColumnGrid {
    pub fn new() -> Self {
        Self::default()
    }

    /// `inSpan` (`server_chunk_space.hpp:54-58`).
    pub fn in_span(&self, x: i32, z: i32) -> bool {
        self.x_origin <= x
            && x < self.x_origin + self.x_size as i32
            && self.z_origin <= z
            && z < self.z_origin + self.z_size as i32
    }

    fn index(&self, x: i32, z: i32) -> usize {
        (x - self.x_origin) as usize + self.x_size * (z - self.z_origin) as usize
    }

    /// The const `operator()` (`server_chunk_space.hpp:47-52`): NULL outside
    /// the span, the column pointer inside.
    pub fn get(&self, x: i32, z: i32) -> Option<&Column> {
        if self.in_span(x, z) {
            self.grid[self.index(x, z)].as_ref()
        } else {
            None
        }
    }

    /// The mutable column, if the cell is in span and created.
    pub fn get_mut(&mut self, x: i32, z: i32) -> Option<&mut Column> {
        if self.in_span(x, z) {
            let index = self.index(x, z);
            self.grid[index].as_mut()
        } else {
            None
        }
    }

    /// The mutable cell — `Column *&` from the reference (`server_chunk_space.hpp:39-45`).
    pub fn slot_mut(&mut self, x: i32, z: i32) -> Option<&mut Option<Column>> {
        if self.in_span(x, z) {
            let index = self.index(x, z);
            self.grid.get_mut(index)
        } else {
            None
        }
    }

    /// `ColumnGrid::rect` (`server_chunk_space.cpp:143-176`): copy in-span
    /// columns into the new rectangle, drop the rest.
    pub fn rect(&mut self, x_origin: i32, z_origin: i32, x_size: usize, z_size: usize) {
        let mut new_grid: Vec<Option<Column>> = (0..x_size * z_size).map(|_| None).collect();
        for x in self.x_origin..self.x_origin + self.x_size as i32 {
            let x_in_span = x >= x_origin && x < x_origin + x_size as i32;
            for z in self.z_origin..self.z_origin + self.z_size as i32 {
                let z_in_span = z >= z_origin && z < z_origin + z_size as i32;
                let index = self.index(x, z);
                let col = self.grid[index].take();
                if x_in_span && z_in_span {
                    new_grid[(x - x_origin) as usize + x_size * (z - z_origin) as usize] = col;
                }
                // A dropped column's destructor smudges its obstacle-holding
                // chunks in the reference; the port has no obstacle surface
                // (registered), so the drop is silent here too.
            }
        }
        self.x_origin = x_origin;
        self.z_origin = z_origin;
        self.x_size = x_size;
        self.z_size = z_size;
        self.grid = new_grid;
    }

    pub fn x_begin(&self) -> i32 {
        self.x_origin
    }

    pub fn x_end(&self) -> i32 {
        self.x_origin + self.x_size as i32
    }

    pub fn z_begin(&self) -> i32 {
        self.z_origin
    }

    pub fn z_end(&self) -> i32 {
        self.z_origin + self.z_size as i32
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mapping::{GeometryMapping, GridBounds};
    use crate::obstacle::test_bsp::slab_obstacle;
    use foundation::math::vector::Vector2;

    fn identity_mapping() -> GeometryMapping {
        GeometryMapping::new(
            "space/",
            100.0,
            GridBounds {
                min_l_grid_x: 0,
                min_l_grid_y: 0,
                max_l_grid_x: 1,
                max_l_grid_y: 1,
                min_grid_x: 0,
                min_grid_y: 0,
                max_grid_x: 1,
                max_grid_y: 1,
            },
            false,
        )
    }

    /// The reference border shape: `bounds_` planes face inward, front side
    /// = chunk interior (`chunk.cpp:695-806`).
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

    fn inside_chunk(identifier: &str, min: Vector3, max: Vector3, items: usize) -> ColumnChunk {
        ColumnChunk {
            identifier: identifier.to_string(),
            volume: (max.x - min.x) * (max.y - min.y) * (max.z - min.z),
            bounding_box: BoundBox::new(min, max),
            static_items: items,
            is_outside: false,
        }
    }

    /// The outside chunk answers a column query on its own; an inside chunk
    /// whose hull covers the point beats it regardless of volume (the first
    /// hull candidate is adopted over `pOutsideChunk_` unconditionally,
    /// `base_chunk_space.cpp:582-583`).
    #[test]
    fn an_inside_hull_beats_the_outside_chunk_at_its_point() {
        let mut column = Column::new();
        let outside = ColumnChunk {
            identifier: "00000000o".into(),
            volume: 100.0 * 20000.0 * 100.0,
            bounding_box: BoundBox::new(
                Vector3::new(0.0, -10000.0, 0.0),
                Vector3::new(100.0, 10000.0, 100.0),
            ),
            static_items: 2,
            is_outside: true,
        };
        let border = box_border(Vector3::new(10.0, 0.0, 10.0), Vector3::new(20.0, 5.0, 20.0));
        column.add_chunk(&border, outside.clone());
        column.add_chunk(
            &border,
            inside_chunk(
                "00000000i",
                Vector3::new(10.0, 0.0, 10.0),
                Vector3::new(20.0, 5.0, 20.0),
                3,
            ),
        );

    fn chunk_map(chunks: Vec<Chunk>) -> BTreeMap<String, Chunk> {
        chunks
            .into_iter()
            .map(|c| (c.identifier.clone(), c))
            .collect()
    }

    #[test]
    fn an_outside_chunk_takes_the_empty_column() {
        let mut column = Column::new(0, 0, 100.0);
        let chunk = outside_chunk("00000000o", 0.0, 1);
        let border = box_border(chunk.bounding_box.min, chunk.bounding_box.max);
        column.add_chunk(&border, &chunk);
        assert_eq!(column.outside_chunk(), Some("00000000o"));
        assert!(!column.has_inside_chunks());
        // the point query answers the outside chunk anywhere in it
        let chunks = chunk_map(vec![chunk]);
        assert_eq!(
            column
                .find_chunk(&Vector3::new(15.0, 2.0, 15.0))
                .unwrap()
                .identifier,
            "00000000i"
        );
        // a re-add while the column is shut to that chunk returns whole:
        // the guard at `base_chunk_space.cpp:430-433` precedes both the slot
        // contest and the holding, so a chunk the pump just shut out of this
        // column adds nothing. A re-add without the see/shut cycle is not
        // that case — `shutIfSeen` only shuts to a chunk the column has seen
        // — and lands a second holding, like the reference's `holdings_`.
        let items_before = column.holdings().len();
        column.open_and_see("00000000o");
        column.shut_if_seen("00000000o");
        column.add_chunk(
            &box_border(
                Vector3::new(0.0, -1.0, 0.0),
                Vector3::new(100.0, 1.0, 100.0),
            ),
            &chunks["00000000o"],
        );
        assert_eq!(
            column.holdings().len(),
            items_before,
            "the shut re-add lands nothing"
        );
        // and the re-add after the pump re-opened the column lands its
        // holding and keeps the slot
        column.open_and_see("00000000o");
        column.add_chunk(
            &box_border(
                Vector3::new(0.0, -1.0, 0.0),
                Vector3::new(100.0, 1.0, 100.0),
            ),
            &chunks["00000000o"],
        );
        assert_eq!(column.holdings().len(), items_before + 1);
        assert_eq!(column.outside_chunk(), Some("00000000o"));
    }

    #[test]
    fn a_larger_outside_chunk_replaces_the_holder() {
        let mut column = Column::new(0, 0, 100.0);
        let small = outside_chunk("00000000o", 0.0, 1);
        column.add_chunk(
            &box_border(small.bounding_box.min, small.bounding_box.max),
            &small,
        );
        let big = outside_chunk("00010000o", 0.0, 5);
        column.add_chunk(
            &box_border(big.bounding_box.min, big.bounding_box.max),
            &big,
        );
        assert_eq!(column.outside_chunk(), Some("00010000o"));
        // a challenger with fewer static items loses its bid: the holder
        // keeps the slot
        let tiny = outside_chunk("00020000o", 0.0, 0);
        column.add_chunk(
            &box_border(tiny.bounding_box.min, tiny.bounding_box.max),
            &tiny,
        );
        assert_eq!(column.outside_chunk(), Some("00010000o"));
        // all three holdings are still recorded, like the reference's
        // holdings_
        assert_eq!(column.holdings().len(), 3);
    }

    #[test]
    fn an_inside_chunk_shares_the_column_with_the_outside_chunk() {
        let mapping = identity_mapping();
        let mut inside = Chunk::new("00000000i", &mapping, &foundation::math::Matrix::identity());
        inside.bounding_box = foundation::math::BoundBox::new(
            Vector3::new(10.0, 0.0, 10.0),
            Vector3::new(20.0, 10.0, 20.0),
        );
        inside.is_loaded = true;

        let mut column = Column::new(0, 0, 100.0);
        let outside = outside_chunk("00000000o", 0.0, 1);
        column.add_chunk(
            &box_border(outside.bounding_box.min, outside.bounding_box.max),
            &outside,
        );
        column.add_chunk(
            &box_border(inside.bounding_box.min, inside.bounding_box.max),
            &inside,
        );
        assert!(column.has_inside_chunks());
        assert_eq!(
            column.holdings_identifiers(),
            vec!["00000000o", "00000000i"]
        );
    }

    /// Among overlapping inside hulls the smallest containing hull wins: the
    /// reference adopts a candidate whenever the incumbent is strictly
    /// bigger (`base_chunk_space.cpp:582-583`), so the walk converges on the
    /// tightest interior.
    #[test]
    fn find_chunk_excluding_skips_the_named_chunk_and_checks_the_box() {
        let mapping = identity_mapping();
        let mut inside = Chunk::new("00000000i", &mapping, &foundation::math::Matrix::identity());
        inside.bounding_box = foundation::math::BoundBox::new(
            Vector3::new(10.0, 0.0, 10.0),
            Vector3::new(20.0, 10.0, 20.0),
        );
        inside.is_loaded = true;
        let outside = outside_chunk("00000000o", 0.0, 1);

        let chunks = chunk_map(vec![outside, inside]);
        let mut column = Column::new(0, 0, 100.0);
        let border = box_border(
            Vector3::new(10.0, 0.0, 10.0),
            Vector3::new(20.0, 10.0, 20.0),
        );
        column.add_chunk(&border, chunks.get("00000000i").unwrap());
        column.add_chunk(
            &box_border(
                chunks.get("00000000o").unwrap().bounding_box.min,
                chunks.get("00000000o").unwrap().bounding_box.max,
            ),
            chunks.get("00000000o").unwrap(),
        );

        // excluding the inside chunk falls back to the outside chunk
        assert_eq!(
            column.find_chunk_excluding(&Vector3::new(15.0, 5.0, 15.0), "00000000i", &chunks),
            Some("00000000o".to_string())
        );
        // excluding nothing finds the inside chunk (its box contains the
        // point)
        assert_eq!(
            column.find_chunk_excluding(&Vector3::new(15.0, 5.0, 15.0), "", &chunks),
            Some("00000000i".to_string())
        );
        // a point in the outside chunk's box but not the shell: excluding
        // the outside chunk leaves nothing that contains it
        assert_eq!(
            column.find_chunk_excluding(&Vector3::new(90.0, 0.0, 90.0), "00000000o", &chunks),
            None
        );
        // the excluded name standing as the current holder leaves the take
        // arm reading the holder's volume — the reference dereferences a
        // stale pointer there (`pChunk->volume()`, base_chunk_space.cpp:549);
        // the port reads the map instead, so a holder whose entry is gone
        // refuses the take and the walk keeps the outside chunk.
        let mut no_inside = chunks.clone();
        no_inside.remove("00000000i");
        assert_eq!(
            column.find_chunk_excluding(&Vector3::new(90.0, 0.0, 90.0), "00000000i", &no_inside),
            Some("00000000o".to_string()),
            "the stale holder loses its bid, the outside chunk stands"
        );
    }

    /// The hull walk prefers the larger volume and skips identifiers no
    /// longer in the space's map; with no outside chunk in the column the
    /// first valid hull wins, and `find_chunk_excluding` holds it against a
    /// smaller challenger it is not excluding.
    #[test]
    fn hull_walks_prefer_larger_volumes_and_skip_stale_ids() {
        let mapping = identity_mapping();
        let mut big = Chunk::new("00000001i", &mapping, &foundation::math::Matrix::identity());
        big.bounding_box = foundation::math::BoundBox::new(
            Vector3::new(10.0, 0.0, 10.0),
            Vector3::new(20.0, 10.0, 20.0),
        );
        big.is_loaded = true;
        let mut small = Chunk::new("00000000i", &mapping, &foundation::math::Matrix::identity());
        small.bounding_box = foundation::math::BoundBox::new(
            Vector3::new(12.0, 0.0, 12.0),
            Vector3::new(16.0, 4.0, 16.0),
        );
        small.is_loaded = true;

        let mut column = Column::new(0, 0, 100.0);
        assert_eq!(column.outside_chunk(), None);

        // a hull whose chunk is gone from the map: its tag is skipped
        let mut ghost = small.clone();
        ghost.identifier = "zzzzzzzzi".to_string();
        column.add_chunk(
            &box_border(Vector3::new(5.0, 0.0, 5.0), Vector3::new(25.0, 20.0, 25.0)),
            &ghost,
        );
        column.add_chunk(
            &box_border(big.bounding_box.min, big.bounding_box.max),
            &big,
        );
        column.add_chunk(
            &box_border(small.bounding_box.min, small.bounding_box.max),
            &small,
            inside_chunk("small_i", Vector3::ZERO, Vector3::new(10.0, 10.0, 10.0), 0),
        );
        column.add_chunk(
            &big,
            inside_chunk("big_i", Vector3::ZERO, Vector3::new(30.0, 30.0, 30.0), 0),
        );

        let chunks = chunk_map(vec![big, small.clone()]);
        let pt = Vector3::new(13.0, 2.0, 13.0);
        // the walk takes the big hull first, then the small hull beats it
        // on the strictly-greater volume rule
        assert_eq!(
            column
                .find_chunk(&Vector3::new(5.0, 5.0, 5.0))
                .unwrap()
                .identifier,
            "small_i",
            "the walk converges on the smallest containing hull"
        );
        // an outside chunk missing from the map loses its bid too: the walk
        // treats the holder like the reference treats a stale pointer and
        // keeps the hull answer
        assert_eq!(
            column.find_chunk(&pt, &chunk_map(vec![small])),
            Some("00000000i".to_string())
        );
        // excluding the winner leaves the big hull standing
        assert_eq!(
            column
                .find_chunk(&Vector3::new(20.0, 20.0, 20.0))
                .unwrap()
                .identifier,
            "big_i"
        );

        // A third, tighter hull takes the shared point over.
        let tiny = box_border(Vector3::new(0.0, 0.0, 0.0), Vector3::new(4.0, 4.0, 4.0));
        column.add_chunk(
            &tiny,
            inside_chunk("tiny_i", Vector3::ZERO, Vector3::new(4.0, 4.0, 4.0), 0),
        );
        assert_eq!(
            column
                .find_chunk(&Vector3::new(2.0, 2.0, 2.0))
                .unwrap()
                .identifier,
            "tiny_i"
        );
    }

    /// `Column::addChunk`'s outside replacement (`base_chunk_space.cpp:433-443`):
    /// the newcomer wins only with strictly more static items; a tie keeps
    /// the incumbent; and the holdings still wrap both originals.
    #[test]
    fn the_seen_bookkeeping_guards_re_adds() {
        let mut column = Column::new(0, 0, 100.0);
        column.open_and_see("a");
        assert!(column.shut_to.is_none());
        column.shut_if_seen("a");
        assert_eq!(column.shut_to.as_deref(), Some("a"));
        // a chunk never seen does not shut the column
        column.shut_if_seen("b");
        assert!(column.shut_to.is_none());
        // open-and-see while shut to the same chunk does not record a
        // second entry; a second open-and-see when not shut does — the
        // reference checks only `shutTo_` (`base_chunk_space.cpp:626-632`)
        column.shut_if_seen("a");
        column.open_and_see("a");
        assert!(column.shut_to.is_none());
        column.open_and_see("a");
        assert_eq!(column.seen, vec!["a".to_string(), "a".to_string()]);
    }

    /// `findChunkExcluding` skips the named chunk everywhere and applies the
    /// bounding-box gate to hull candidates (`base_chunk_space.cpp:552-557`).
    #[test]
    fn the_grid_window_moves_and_drops_columns() {
        let mut grid = ColumnGrid::new();
        grid.rect(0, 0, 2, 2, &mut Vec::new());
        grid.set(1, 1, Some(Column::new(0, 0, 100.0)));
        assert!(grid.in_span(1, 1));
        assert!(!grid.in_span(2, 1));
        assert!(grid.get(1, 1).is_some());
        assert!(grid.get(0, 0).is_none());

        let point = Vector3::new(5.0, 5.0, 5.0);
        assert_eq!(
            column
                .find_chunk_excluding(&point, "nothing")
                .unwrap()
                .identifier,
            "00000000i"
        );
        // Excluded: the outside chunk takes over again.
        assert_eq!(
            column
                .find_chunk_excluding(&point, "00000000i")
                .unwrap()
                .identifier,
            "00000000o"
        );
        // Excluded with no other candidate: None.
        let only = Column::new();
        assert!(only.find_chunk_excluding(&point, "00000000o").is_none());
        // A point outside the hull's bounding box never adopts it.
        assert_eq!(
            column
                .find_chunk(&Vector3::new(50.0, 0.0, 50.0))
                .unwrap()
                .identifier,
            "00000000o"
        );

        // Excluding the outside chunk leaves the inside chunk answering
        // (`pOutsideChunk_ != pNot`, `base_chunk_space.cpp:545-547`).
        assert_eq!(
            column
                .find_chunk_excluding(&point, "00000000o")
                .unwrap()
                .identifier,
            "00000000i"
        );

        // A candidate whose hull covers the point but whose bounding box
        // does not is never adopted (the box gate,
        // `base_chunk_space.cpp:554-557`).
        let mut boxless = Column::new();
        let mut shell = inside_chunk("boxless_i", Vector3::ZERO, Vector3::new(1.0, 1.0, 1.0), 0);
        shell.bounding_box = BoundBox::new(
            Vector3::new(-1000.0, -1000.0, -1000.0),
            Vector3::new(-900.0, 900.0, 900.0),
        );
        boxless.add_chunk(
            &box_border(Vector3::new(0.0, 0.0, 0.0), Vector3::new(10.0, 10.0, 10.0)),
            shell,
        );
        assert!(boxless.find_chunk_excluding(&point, "nobody").is_none());

        // Every focus holding of a chunk is its own entry in `holdings_`.
        let mut counted = Column::new();
        counted.add_chunk(&Vec::new(), outside.clone());
        counted.add_chunk(&Vec::new(), outside.clone());
        counted.add_chunk(&Vec::new(), outside);
        assert_eq!(counted.holdings_of("00000000o"), 3);
        assert_eq!(counted.holdings_of("never_o"), 0);
    }

    /// `findChunkExcluding` compares an adopted hull candidate against the
    /// outside chunk by identity, not volume
    /// (`base_chunk_space.cpp:545-557`): the outside chunk stays the start
    /// whenever it is not the excluded one, and after that only the
    /// strictly bigger incumbent adopts.
    #[test]
    fn find_chunk_excluding_walks_the_volume_rule_and_keeps_the_outside() {
        let mut column = Column::new();
        let outside = ColumnChunk {
            identifier: "00000000o".into(),
            volume: 100.0 * 20000.0 * 100.0,
            bounding_box: BoundBox::new(
                Vector3::new(0.0, -10000.0, 0.0),
                Vector3::new(100.0, 10000.0, 100.0),
            ),
            static_items: 0,
            is_outside: true,
        };
        column.add_chunk(&Vec::new(), outside);
        let point = Vector3::new(5.0, 5.0, 5.0);
        let big = box_border(Vector3::new(0.0, 0.0, 0.0), Vector3::new(20.0, 20.0, 20.0));
        let small = box_border(Vector3::new(0.0, 0.0, 0.0), Vector3::new(6.0, 6.0, 6.0));
        column.add_chunk(
            &big,
            inside_chunk("big_i", Vector3::ZERO, Vector3::new(20.0, 20.0, 20.0), 0),
        );
        column.add_chunk(
            &small,
            inside_chunk("small_i", Vector3::ZERO, Vector3::new(6.0, 6.0, 6.0), 0),
        );

        // Nothing excluded: the tightest hull answers, the outside chunk is
        // adopted over by the first candidate.
        assert_eq!(
            column
                .find_chunk_excluding(&point, "nobody")
                .unwrap()
                .identifier,
            "small_i"
        );
        // The tightest one out of the way: the bigger hull answers, which is
        // the incumbent-vs-candidate comparison with an adopted incumbent.
        assert_eq!(
            column
                .find_chunk_excluding(&point, "small_i")
                .unwrap()
                .identifier,
            "big_i"
        );
    }

    /// `openAndSee`/`shutIfSeen` (`base_chunk_space.cpp:626-652`): an open
    /// records the chunk and clears the shut state; a shut takes hold only
    /// for a chunk that was seen; and a shut column refuses the chunk's
    /// holding in `add_chunk`.
    #[test]
    fn open_and_shut_drive_the_holdings_gate() {
        let mut column = Column::new();
        // Not seen: the shut is a clear, not a hold.
        column.shut_if_seen("00000000o");
        assert!(!column.is_shut_to("00000000o"));

        column.open_and_see("00000000o");
        assert!(column.has_seen("00000000o"));
        assert!(!column.is_shut_to("00000000o"));
        column.shut_if_seen("00000000o");
        assert!(column.is_shut_to("00000000o"));

        // While shut to the chunk, add_chunk returns without a holding.
        let outside = ColumnChunk {
            identifier: "00000000o".into(),
            volume: 1.0,
            bounding_box: BoundBox::new(Vector3::ZERO, Vector3::new(1.0, 1.0, 1.0)),
            static_items: 0,
            is_outside: true,
        };
        column.add_chunk(&Vec::new(), outside);
        assert!(column.holdings_identifiers().is_empty());

        // Re-opening clears the shut state (no duplicate seen entry).
        column.open_and_see("00000000o");
        assert!(!column.is_shut_to("00000000o"));
        column.open_and_see("00000001o");
        assert_eq!(column.seen_len(), 2);
    }

    /// `ColumnGrid::rect` (`server_chunk_space.cpp:143-176`): in-span columns
    /// move to their new indices, out-of-span columns are dropped.
    #[test]
    fn grid_rect_moves_and_drops_columns() {
        let mut grid = ColumnGrid::new();
        grid.rect(0, 0, 2, 1, &mut Vec::new());
        grid.set(1, 0, Some(Column::new(0, 0, 100.0)));
        let mut dropped = Vec::new();
        grid.rect(1, 0, 2, 1, &mut dropped);
        assert!(dropped.is_empty());
        assert!(grid.get(1, 0).is_some());
        assert!(grid.get(2, 0).is_none(), "outside the span is None");

        // Shift to x 1..3: column (1,0) moves, column (0,0) drops.
        grid.rect(1, 0, 2, 1);
        assert!(grid.get(0, 0).is_none());
        assert!(grid.get(1, 0).is_some());
        assert!(grid.get(2, 0).is_none());
        assert_eq!((grid.x_begin(), grid.x_end()), (1, 3));
    }

    /// A `Chunk` snapshot carries the volume and static-item face the
    /// column queries compare (`Chunk::volume`, `chunk.cpp:3506-3514`).
    #[test]
    fn column_chunk_snapshot_copies_the_query_face() {
        let mapping = identity_mapping();
        let chunk = Chunk::new("00010000o", &mapping, &Matrix::identity());
        let snap = ColumnChunk::from_chunk(&chunk);
        assert_eq!(snap.identifier, "00010000o");
        assert!(snap.is_outside);
        // 100 x 20000 x 100 for an outside chunk of grid size 100.
        assert_eq!(snap.volume, 100.0 * 20000.0 * 100.0);
        assert_eq!(snap.static_items, 0);
    }

    /// Every element a line through the column's tree (1,*,1)-(3,*,3)
    /// touches — the slab obstacle's cells.
    fn tree_elements(column: &Column) -> Vec<u32> {
        let mut out = Vec::new();
        let mut traversal = column.obstacles().traverse(
            &Vector3::new(1.0, 0.0, 1.0),
            &Vector3::new(3.0, 0.0, 3.0),
            0.0,
        );
        while let Some(element) = traversal.next() {
            out.push(element);
        }
        out
    }

    /// The obstacle tree is the reference ctor's: placed at the column's
    /// grid cell in world coordinates, shipped depth, the space's grid
    /// size as its range.
    #[test]
    fn the_ctor_builds_the_obstacle_tree_at_the_grid_point() {
        let column = Column::new(3, -2, 100.0);

        assert_eq!(column.obstacles().origin(), Vector2::new(300.0, -200.0));
        assert_eq!(column.obstacles().depth(), OBSTACLE_TREE_DEPTH);
        assert_eq!(column.obstacles().range(), 100.0);
        assert!(tree_elements(&column).is_empty());
    }

    /// `Column::addObstacle`: the arena index lands in the tree, computed
    /// off the obstacle's world box.
    #[test]
    fn add_obstacle_lands_the_arena_index() {
        let mut column = Column::new(0, 0, 100.0);
        let obstacle = slab_obstacle(Some("001x000x"));

        column.add_obstacle(&obstacle, 7);

        assert_eq!(tree_elements(&column), vec![7]);
    }

    /// A column shut to a chunk refuses that chunk's obstacles — and
    /// still takes another chunk's.
    #[test]
    fn a_column_shut_to_the_chunk_refuses_its_obstacles() {
        let mut column = Column::new(0, 0, 100.0);
        column.open_and_see("001x000x");
        column.shut_if_seen("001x000x");

        column.add_obstacle(&slab_obstacle(Some("001x000x")), 1);
        assert!(tree_elements(&column).is_empty());

        column.add_obstacle(&slab_obstacle(Some("00000000i")), 2);
        assert_eq!(tree_elements(&column), vec![2]);

        // a column with no shutoff takes every obstacle
        let mut open = Column::new(0, 0, 100.0);
        open.add_obstacle(&slab_obstacle(Some("001x000x")), 3);
        assert_eq!(tree_elements(&open), vec![3]);
    }
}
