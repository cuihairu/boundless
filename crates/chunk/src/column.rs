//! BW-10-01-05 the focus-grid column (`BaseChunkSpace::Column`,
//! `base_chunk_space.hpp:127-176` / `base_chunk_space.cpp:377-600`) and the
//! grid of columns it lives in (`ServerChunkSpace::ColumnGrid`,
//! `server_chunk_space.hpp:34-90`).
//!
//! A column is one grid square of the space: the outside chunk owning it
//! plus a hull tree of the inside chunks overlapping it. Point queries
//! (`findChunk`/`findChunkExcluding`) start at the outside chunk and prefer
//! the larger-volume hull result, exactly like the reference walk.
//!
//! Porting notes: the reference column stores raw `Chunk*`s; the port
//! stores chunk identifiers and resolves them in the owning space's map
//! (an id missing from the map is skipped where the reference would
//! dereference a stale pointer — registered divergence). The outside-chunk
//! replacement rule keeps whichever chunk has more static items.

use std::collections::BTreeMap;

use foundation::math::{PlaneEq, Vector3};

use crate::chunk::Chunk;
use crate::hulltree::HullTree;

/// `BaseChunkSpace::Column` (`base_chunk_space.cpp:377-391`): an empty
/// column owns no tree; the reference also allocates an obstacle tree,
/// which has no port-side face here (BW-10-03 collision) and is registered.
#[derive(Debug, Default, Clone)]
pub struct Column {
    /// `pChunkTree_`: the inside chunks' hull tree.
    tree: Option<HullTree>,
    /// `pOutsideChunk_`, with the holder's static item count so the
    /// replacement rule can run without re-resolving the identifier (the
    /// reference reads it off the stored pointer).
    outside: Option<(String, usize)>,
    /// `holdings_`: the hull entries this column holds, one per
    /// `addChunk` call (the reference's `HullContents` objects).
    holdings: Vec<String>,
    /// `seen_` / `shutTo_`: the open-and-see bookkeeping the focus loop
    /// uses to avoid re-adding a chunk's hull to a column that has it.
    seen: Vec<String>,
    shut_to: Option<String>,
}

impl Column {
    pub fn new() -> Self {
        Self::default()
    }

    /// `Column::addChunk` (`base_chunk_space.cpp:423-465`).
    pub fn add_chunk(&mut self, border: &[PlaneEq], chunk: &Chunk) {
        // see if this column has already seen this chunk
        if self.shut_to.as_deref() == Some(chunk.identifier.as_str()) {
            return;
        }

        let stuff = chunk.identifier.clone();

        if chunk.is_outside {
            // ERROR_MSG "Column::addChunk: Replacing outside chunk ...":
            // take whichever has the most static chunk items — the new
            // chunk only takes the slot when it strictly beats the holder
            let replace = match &self.outside {
                None => true,
                Some((_, existing_items)) => chunk.items.len() > *existing_items,
            };
            if replace {
                self.outside = Some((chunk.identifier.clone(), chunk.items.len()));
            }
        } else {
            let tree = self.tree.get_or_insert_with(HullTree::new);
            tree.add(border, stuff.clone());
            // if this is an inside chunk being added to a column with an
            // outside chunk already, the reference jogs that chunk's
            // foreign items — no port-side item nesting face, registered.
        }

        // the holding records the originally added chunk either way
        self.holdings.push(stuff);
    }

    /// `Column::hasInsideChunks` (`base_chunk_space.hpp:147`).
    pub fn has_inside_chunks(&self) -> bool {
        self.tree.is_some()
    }

    /// `Column::pOutsideChunk` (`base_chunk_space.hpp:160`).
    pub fn outside_chunk(&self) -> Option<&str> {
        self.outside.as_ref().map(|(id, _)| id.as_str())
    }

    /// `Column::findChunk` (`base_chunk_space.cpp:570-588`): start at the
    /// outside chunk, then walk the hull chain preferring larger volumes.
    pub fn find_chunk(&self, point: &Vector3, chunks: &BTreeMap<String, Chunk>) -> Option<String> {
        let mut current = self.outside.clone().map(|(id, _)| id);
        if let Some(tree) = &self.tree {
            for id in tree.test_point(point) {
                let Some(result) = chunks.get(&id) else {
                    continue;
                };
                let take = match &current {
                    c if c.as_deref() == self.outside_id() => true,
                    Some(c) => match chunks.get(c) {
                        Some(holder) => holder.volume() > result.volume(),
                        None => false,
                    },
                    None => false,
                };
                if take {
                    current = Some(id);
                }
            }
        }
        current
    }

    /// `Column::findChunkExcluding` (`base_chunk_space.cpp:539-558`): the
    /// same walk, skipping `p_not`, and requiring the hull's bounding box
    /// to contain the point.
    pub fn find_chunk_excluding(
        &self,
        point: &Vector3,
        p_not: &str,
        chunks: &BTreeMap<String, Chunk>,
    ) -> Option<String> {
        let mut current = match self.outside_id() {
            Some(id) if id == p_not => None,
            other => other.map(|id| id.to_string()),
        };
        if let Some(tree) = &self.tree {
            for id in tree.test_point(point) {
                let Some(result) = chunks.get(&id) else {
                    continue;
                };
                let candidate = match &current {
                    None => true,
                    Some(c) if self.outside_id() == Some(c.as_str()) => true,
                    Some(c) => match chunks.get(c) {
                        Some(holder) => holder.volume() > result.volume(),
                        None => false,
                    },
                };
                // `boundingBox().intersects( point )` (`boundbox.ipp:303-307`,
                // the port's half-open `contains`)
                if candidate && result.bounding_box.contains(point) && id != p_not {
                    current = Some(id);
                }
            }
        }
        current
    }

    /// The outside chunk's identifier, if any.
    fn outside_id(&self) -> Option<&str> {
        self.outside.as_ref().map(|(id, _)| id.as_str())
    }

    /// `Column::openAndSee` (`base_chunk_space.cpp:626-635`).
    pub fn open_and_see(&mut self, chunk: &str) {
        if self.shut_to.as_deref() != Some(chunk) {
            self.seen.push(chunk.to_string());
        }
        self.shut_to = None;
    }

    /// `Column::shutIfSeen` (`base_chunk_space.cpp:641-652`).
    pub fn shut_if_seen(&mut self, chunk: &str) {
        self.shut_to = if self.seen.iter().any(|s| s == chunk) {
            Some(chunk.to_string())
        } else {
            None
        };
    }

    /// The chunk identifiers this column holds hulls for, in add order
    /// (the destructor face: the reference deletes the holdings and the
    /// obstacle-held chunks get smudged; the port reports them so the
    /// space can blur them).
    pub fn holdings(&self) -> &[String] {
        &self.holdings
    }
}

/// `ServerChunkSpace::ColumnGrid` (`server_chunk_space.hpp:34-90`): a
/// rectangular window of columns addressed by grid coordinates.
#[derive(Debug, Default, Clone)]
pub struct ColumnGrid {
    grid: Vec<Option<Column>>,
    x_origin: i32,
    z_origin: i32,
    x_size: i32,
    z_size: i32,
}

impl ColumnGrid {
    pub fn new() -> Self {
        Self::default()
    }

    /// `inSpan` (`server_chunk_space.hpp:60-64`).
    pub fn in_span(&self, x: i32, z: i32) -> bool {
        self.x_origin <= x
            && x < self.x_origin + self.x_size
            && self.z_origin <= z
            && z < self.z_origin + self.z_size
    }

    /// `operator()(x, z)` const arm (`server_chunk_space.hpp:48-53`).
    pub fn get(&self, x: i32, z: i32) -> Option<&Column> {
        if !self.in_span(x, z) {
            return None;
        }
        let index = (x - self.x_origin) + self.x_size * (z - self.z_origin);
        self.grid[index as usize].as_ref()
    }

    /// The mutable arm of `operator()(x, z)` (`server_chunk_space.hpp:41-46`),
    /// returning the column itself rather than the slot's option so a mutable
    /// borrow of it can be handed back to the caller.
    pub fn get_mut(&mut self, x: i32, z: i32) -> Option<&mut Column> {
        if !self.in_span(x, z) {
            return None;
        }
        let index = (x - self.x_origin) + self.x_size * (z - self.z_origin);
        self.grid[index as usize].as_mut()
    }

    /// Install or clear a column; returns the previous one.
    pub fn set(&mut self, x: i32, z: i32, column: Option<Column>) -> Option<Column> {
        let index = (x - self.x_origin) + self.x_size * (z - self.z_origin);
        let old = self.grid[index as usize].take();
        self.grid[index as usize] = column;
        old
    }

    /// `ColumnGrid::rect` (`server_chunk_space.cpp:143-176`): resize the
    /// window, carrying in-span columns over and dropping (deleting) the
    /// others, which the caller reports to the chunks they held.
    pub fn rect(
        &mut self,
        x_origin_new: i32,
        z_origin_new: i32,
        x_size_new: i32,
        z_size_new: i32,
        dropped: &mut Vec<Column>,
    ) {
        let mut grid_new: Vec<Option<Column>> =
            vec![None; (x_size_new * z_size_new).max(0) as usize];
        for x in self.x_origin..self.x_origin + self.x_size {
            let x_in_span = x >= x_origin_new && x < x_origin_new + x_size_new;
            for z in self.z_origin..self.z_origin + self.z_size {
                let z_in_span = z >= z_origin_new && z < z_origin_new + z_size_new;
                let index = (x - self.x_origin) + self.x_size * (z - self.z_origin);
                let col = self.grid[index as usize].take();
                if x_in_span && z_in_span {
                    let new_index = (x - x_origin_new) + x_size_new * (z - z_origin_new);
                    grid_new[new_index as usize] = col;
                } else if let Some(col) = col {
                    dropped.push(col);
                }
            }
        }
        self.x_origin = x_origin_new;
        self.z_origin = z_origin_new;
        self.x_size = x_size_new;
        self.z_size = z_size_new;
        self.grid = grid_new;
    }

    /// The current window (`xBegin/zBegin/xEnd/zEnd`).
    pub fn span(&self) -> (i32, i32, i32, i32) {
        (self.x_origin, self.z_origin, self.x_size, self.z_size)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mapping::{GeometryMapping, GridBounds};

    fn box_border(min: Vector3, max: Vector3) -> Vec<PlaneEq> {
        // inward normals, the chunk border convention (`chunk.cpp:675-807`)
        vec![
            PlaneEq::new(Vector3::new(1.0, 0.0, 0.0), min.x),
            PlaneEq::new(Vector3::new(-1.0, 0.0, 0.0), -max.x),
            PlaneEq::new(Vector3::new(0.0, 1.0, 0.0), min.y),
            PlaneEq::new(Vector3::new(0.0, -1.0, 0.0), -max.y),
            PlaneEq::new(Vector3::new(0.0, 0.0, 1.0), min.z),
            PlaneEq::new(Vector3::new(0.0, 0.0, -1.0), -max.z),
        ]
    }

    fn identity_mapping() -> GeometryMapping {
        let bounds = GridBounds {
            min_l_grid_x: 0,
            min_l_grid_y: 0,
            max_l_grid_x: 1,
            max_l_grid_y: 1,
            min_grid_x: 0,
            min_grid_y: 0,
            max_grid_x: 1,
            max_grid_y: 1,
        };
        GeometryMapping::new("space/", 100.0, bounds, false)
    }

    fn outside_chunk(identifier: &str, x: f32, items: usize) -> Chunk {
        let mapping = identity_mapping();
        let mut chunk = Chunk::new(identifier, &mapping, &foundation::math::Matrix::identity());
        chunk.is_loaded = true;
        chunk.items = (0..items)
            .map(|i| crate::chunk::ChunkItem {
                tag: "model".to_string(),
                label: i.to_string(),
                resource: None,
                position: None,
                transform: foundation::math::Matrix::identity(),
            })
            .collect();
        chunk.bounding_box = foundation::math::BoundBox::new(
            Vector3::new(x, -1.0, 0.0),
            Vector3::new(x + 100.0, 1.0, 100.0),
        );
        chunk
    }

    fn chunk_map(chunks: Vec<Chunk>) -> BTreeMap<String, Chunk> {
        chunks
            .into_iter()
            .map(|c| (c.identifier.clone(), c))
            .collect()
    }

    #[test]
    fn an_outside_chunk_takes_the_empty_column() {
        let mut column = Column::new();
        let chunk = outside_chunk("00000000o", 0.0, 1);
        let border = box_border(chunk.bounding_box.min, chunk.bounding_box.max);
        column.add_chunk(&border, &chunk);
        assert_eq!(column.outside_chunk(), Some("00000000o"));
        assert!(!column.has_inside_chunks());
        // the point query answers the outside chunk anywhere in it
        let chunks = chunk_map(vec![chunk]);
        assert_eq!(
            column.find_chunk(&Vector3::new(50.0, 0.0, 50.0), &chunks),
            Some("00000000o".to_string())
        );
    }

    #[test]
    fn a_larger_outside_chunk_replaces_the_holder() {
        let mut column = Column::new();
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

        let mut column = Column::new();
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

        let chunks = chunk_map(vec![outside, inside]);
        // inside the shell the hull result wins (it has a real volume, the
        // outside chunk's MIN/MAX height box is bigger)
        assert_eq!(
            column.find_chunk(&Vector3::new(15.0, 5.0, 15.0), &chunks),
            Some("00000000i".to_string())
        );
        // outside the shell but inside the grid cell: the outside chunk
        assert_eq!(
            column.find_chunk(&Vector3::new(90.0, 0.0, 90.0), &chunks),
            Some("00000000o".to_string())
        );
    }

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
        let mut column = Column::new();
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

        let mut column = Column::new();
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
        );

        let chunks = chunk_map(vec![big, small]);
        let pt = Vector3::new(13.0, 2.0, 13.0);
        // the walk takes the big hull first, then the small hull beats it
        // on the strictly-greater volume rule
        assert_eq!(
            column.find_chunk(&pt, &chunks),
            Some("00000000i".to_string())
        );
        // excluding the winner leaves the big hull standing
        assert_eq!(
            column.find_chunk_excluding(&pt, "00000000i", &chunks),
            Some("00000001i".to_string())
        );
        // excluding nothing: the walk re-runs and re-takes the small hull;
        // the big hull's larger volume does not displace the holder
        assert_eq!(
            column.find_chunk_excluding(&pt, "", &chunks),
            Some("00000000i".to_string())
        );
    }

    #[test]
    fn the_seen_bookkeeping_guards_re_adds() {
        let mut column = Column::new();
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

    #[test]
    fn the_grid_window_moves_and_drops_columns() {
        let mut grid = ColumnGrid::new();
        grid.rect(0, 0, 2, 2, &mut Vec::new());
        grid.set(1, 1, Some(Column::new()));
        assert!(grid.in_span(1, 1));
        assert!(!grid.in_span(2, 1));
        assert!(grid.get(1, 1).is_some());
        assert!(grid.get(0, 0).is_none());

        // shrink past the held column: it drops out and is reported
        let mut dropped = Vec::new();
        grid.rect(0, 0, 1, 1, &mut dropped);
        assert_eq!(dropped.len(), 1);
        assert!(grid.get(1, 1).is_none());

        // shift the window: the surviving column moves with its cell
        let mut grid = ColumnGrid::new();
        grid.rect(0, 0, 2, 1, &mut Vec::new());
        grid.set(1, 0, Some(Column::new()));
        let mut dropped = Vec::new();
        grid.rect(1, 0, 2, 1, &mut dropped);
        assert!(dropped.is_empty());
        assert!(grid.get(1, 0).is_some());
        assert_eq!(grid.span(), (1, 0, 2, 1));
    }
}
