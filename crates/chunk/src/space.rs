//! BW-10-01-03 chunk space streaming: the focus scan and the bind sequence.
//!
//! A port of the server-relevant slices of `ChunkManager::scan`
//! (`chunk_manager.cpp:2132-2222`) and `Chunk::bind`/`bindPortals`
//! (`chunk.cpp:1039-1288`). The scan walks grid offsets around the camera
//! cell sorted by squared radius (`GridComp`), skipping cells outside the
//! mapping's world bounds, creating the outside `Chunk` objects and returning
//! the ones still to load in that order. `bind` then connects a loaded
//! chunk's unbound portals to the neighbour chunks that are already bound,
//! two-way, exactly like `bindPortals` plus `Chunk::bind(Chunk*)`.
//!
//! The spatial index (BW-10-01-05) sits over the same map: the focus grid of
//! [`Column`]s (`crate::column`), one per grid square, each holding the
//! outside chunk of the square plus a hull tree of the inside chunks that
//! overlap it. Chunks enter the index when they are focussed; point queries
//! walk the column instead of scanning the map.
//!
//! Registered divergences (see `docs/review/bw10-01-chunk-load.md`): the
//! within-equal-radius order of the reference `std::sort` is unspecified, so
//! this port pins a deterministic tie-break (radius, then offset x, then z)
//! and the tests assert the radius order and cell set, not a brittle per-tier
//! sequence; `resolveExtern`/invasive portal resolution need the space column
//! (`guessChunk`) and `ChunkOverlappers` belong to the scene-adaptation
//! surface (BW-10-02); `gridToLocal` is the identity (transformed mappings
//! are BW-10-02+); unload is BW-10-01-04; and where the reference stores raw
//! `Chunk*`s the port stores identifiers, so a hull tag whose chunk has left
//! the map is skipped instead of dereferenced.

use std::collections::BTreeMap;

use foundation::math::{Matrix, PlaneEq, Vector3};
use resources::datasection::DataSection;

use crate::boundary::PortalTarget;
use crate::chunk::Chunk;
use crate::column::{Column, ColumnGrid};
use crate::mapping::GeometryMapping;

/// `MAX_WORTHY_CHUNKS` (`chunk_manager.cpp:70`): a scan visits at most this
/// many not-yet-loaded chunks.
const MAX_WORTHY_CHUNKS: usize = 50;

/// `ChunkSpace`: the chunk map and the streaming state of one space. The
/// reference keeps this in `ChunkSpace::ChunkMap` plus the manager's scan
/// state; the port stores the loaded chunks and the bind-order trace.
#[derive(Debug, Clone)]
pub struct ChunkSpace {
    pub id: u16,
    pub mappings: Vec<GeometryMapping>,
    /// `ChunkMap`, keyed by identifier (`ChunkSpace::addChunk`).
    chunks: BTreeMap<String, Chunk>,
    /// The order identifiers became bound — the BW-10-01-03 acceptance trace.
    bind_order: Vec<String>,
    /// `currentFocus_` (`server_chunk_space.hpp:88`): the grid window of
    /// the spatial index, one column per grid square of the space.
    focus: ColumnGrid,
    /// `blurred_` (`base_chunk_space.hpp:236`): bound chunks whose hull is
    /// out of the focus grid, waiting for [`ChunkSpace::focus`].
    blurred: Vec<String>,
}

impl ChunkSpace {
    pub fn new(id: u16, mappings: Vec<GeometryMapping>) -> Self {
        let mut space = Self {
            id,
            mappings,
            chunks: BTreeMap::new(),
            bind_order: Vec::new(),
            focus: ColumnGrid::new(),
            blurred: Vec::new(),
        };
        // `ChunkSpace::ChunkSpace` sizes the focus grid from the mappings
        // (`recalcGridBounds`); without it no cell is in span and every
        // column lookup misses.
        space.recalc_grid_bounds();
        space
    }

    /// The space's grid size (`ChunkSpace::gridSize`), from its mapping; a
    /// space with no mapping yet answers the default (`BaseChunkSpace`'s
    /// `gridSize_` starts at `DEFAULT_GRID_SIZE`).
    pub fn grid_size(&self) -> f32 {
        self.mappings
            .first()
            .map_or(crate::mapping::DEFAULT_GRID_SIZE, |m| m.grid_size)
    }

    /// `pointToGrid` (`base_chunk_space.hpp:186-189`).
    pub fn point_to_grid(&self, point: f32) -> i32 {
        (point / self.grid_size()).floor() as i32
    }

    /// `gridToPoint` (`base_chunk_space.hpp:181-184`).
    pub fn grid_to_point(&self, grid: i32) -> f32 {
        grid as f32 * self.grid_size()
    }

    /// `addChunk` (`chunk_space.cpp`): register a chunk in the space's map.
    ///
    /// The port also focusses the chunk into the index, so the map and the
    /// index can never disagree (the reference defers the hull insertion to
    /// the focus pump driven by `noticeChunk`; see [`ChunkSpace::focus`]).
    pub fn add_chunk(&mut self, chunk: Chunk) {
        let identifier = chunk.identifier.clone();
        self.chunks.insert(identifier.clone(), chunk);
        self.focus_chunk_into_columns(&identifier);
    }

    /// `findChunkByGrid` (`chunk_manager.cpp:2199`): the outside chunk owning
    /// a local grid cell, if it exists in the map.
    pub fn find_chunk_by_grid(&self, grid_x: i32, grid_z: i32) -> Option<&Chunk> {
        let id = self.mappings[0].outside_chunk_identifier(grid_x, grid_z, false);
        self.chunks.get(&id)
    }

    pub fn chunk(&self, identifier: &str) -> Option<&Chunk> {
        self.chunks.get(identifier)
    }

    pub fn chunk_mut(&mut self, identifier: &str) -> Option<&mut Chunk> {
        self.chunks.get_mut(identifier)
    }

    pub fn chunks(&self) -> impl Iterator<Item = &Chunk> {
        self.chunks.values()
    }

    /// The identifiers in bind order (BW-10-01-03 acceptance).
    pub fn bind_order(&self) -> &[String] {
        &self.bind_order
    }

    // ---- the spatial index (BW-10-01-05) --------------------------------

    /// The focus grid (`currentFocus_`).
    pub fn focus_grid(&self) -> &ColumnGrid {
        &self.focus
    }

    /// The blurred queue: the chunk identifiers waiting for [`focus`].
    pub fn blurred(&self) -> &[String] {
        &self.blurred
    }

    /// `ChunkSpace::column(point, canCreate)` (`chunk_space.cpp:784-800`):
    /// the column owning the point's grid square, created when asked.
    pub fn column(&mut self, point: &Vector3, can_create: bool) -> Option<&Column> {
        let x = self.point_to_grid(point.x);
        let z = self.point_to_grid(point.z);
        if !self.focus.in_span(x, z) {
            return None;
        }
        if can_create && self.focus.get_mut(x, z).is_none() {
            self.focus.set(x, z, Some(Column::new()));
        }
        self.focus.get(x, z)
    }

    /// The column owning the point's grid square, without creating one: the
    /// reading face the point queries use.
    pub fn column_at(&self, point: &Vector3) -> Option<&Column> {
        let x = self.point_to_grid(point.x);
        let z = self.point_to_grid(point.z);
        self.focus.get(x, z)
    }

    /// `recalcGridBounds` (`chunk_space.cpp:1077-1130` +
    /// `server_chunk_space.cpp:137-142`): size the focus grid window to the
    /// union of the mappings' world grid bounds. The port carries the window
    /// rather than the raw min/max, so a resize hands the displaced columns'
    /// holdings to the caller to blur (the reference deletes them, which
    /// smudges the chunks — `Chunk::smudge` → `blurredChunk`).
    pub fn recalc_grid_bounds(&mut self) {
        let (min_x, min_z, max_x, max_z) = self.mappings.iter().fold(
            (i32::MAX, i32::MAX, i32::MIN, i32::MIN),
            |(min_x, min_z, max_x, max_z), m| {
                (
                    min_x.min(m.bounds.min_grid_x),
                    min_z.min(m.bounds.min_grid_y),
                    max_x.max(m.bounds.max_grid_x),
                    max_z.max(m.bounds.max_grid_y),
                )
            },
        );
        let empty = min_x == i32::MAX;
        let (origin_x, origin_z, width, depth) = if empty {
            // `ChunkSpace::recalcGridBounds`'s empty-mappings branch.
            (0, 0, 0, 0)
        } else {
            (min_x, min_z, max_x - min_x + 1, max_z - min_z + 1)
        };
        let mut dropped = Vec::new();
        self.focus
            .rect(origin_x, origin_z, width, depth, &mut dropped);
        let mut reblur = Vec::new();
        for column in dropped {
            reblur.extend(column.holdings().iter().cloned());
        }
        for id in reblur {
            self.blur_chunk(&id);
        }
    }

    /// `Chunk::focus`'s cell set (`chunk.cpp:1566-1596`): the outside chunk
    /// goes to the column of its centre, an inside chunk to the columns of
    /// its eight bounding-box corners, out-of-span cells skipped.
    fn focus_cells(&self, chunk: &Chunk) -> Vec<(i32, i32)> {
        let mut cells = Vec::new();
        if chunk.is_outside {
            let cell = chunk_index_cell(&chunk.centre(), self.grid_size());
            // `column(centre)` returns NULL out of span, and the reference
            // inserts and then filters that null out of the column set.
            if self.focus.in_span(cell.0, cell.1) {
                cells.push(cell);
            }
            return cells;
        }
        let bb = &chunk.bounding_box;
        for i in 0..8 {
            let corner = Vector3::new(
                if i & 1 != 0 { bb.max.x } else { bb.min.x },
                if i & 2 != 0 { bb.max.y } else { bb.min.y },
                if i & 4 != 0 { bb.max.z } else { bb.min.z },
            );
            let cell = chunk_index_cell(&corner, self.grid_size());
            if !self.focus.in_span(cell.0, cell.1) || cells.contains(&cell) {
                continue;
            }
            cells.push(cell);
        }
        cells
    }

    /// `Chunk::focus` (`chunk.cpp:1547-1612`): insert the chunk's hull border
    /// into every column of its cell set. `focusCount_` becomes one, which is
    /// what the reference's `completed()` reads.
    fn focus_chunk_into_columns(&mut self, identifier: &str) {
        let Some(chunk) = self.chunks.get(identifier) else {
            return;
        };
        let border = chunk_index_border(chunk);
        let cells = self.focus_cells(chunk);
        for (x, z) in cells {
            if self.focus.get_mut(x, z).is_none() {
                self.focus.set(x, z, Some(Column::new()));
            }
            // The two `let else` arms are borrow fences, not failure paths:
            // the cells are all in span (`focus_cells` filtered them) and
            // the chunk is still in the map (the early return above), so
            // both acquisitions succeed — but the reference hands the raw
            // pointers straight to `addChunk` and the port's field split
            // cannot prove it.
            if let (Some(column), Some(chunk)) =
                (self.focus.get_mut(x, z), self.chunks.get(identifier))
            {
                column.add_chunk(&border, chunk);
            }
        }
        if let Some(chunk) = self.chunks.get_mut(identifier) {
            chunk.focus_count = 1;
        }
    }

    /// `ServerChunkSpace::focus` (`server_chunk_space.cpp:36-100`): the
    /// focus pump. Every blurred chunk is shut out of the nine columns
    /// around its centre, refocussed — which re-adds its hull to the columns
    /// it overlaps now — and seen by them again. The reference runs this
    /// from the camera update every frame; the port runs it after a batch of
    /// bindings, the sync form the single-threaded loader implies.
    pub fn focus(&mut self) {
        let mut blurred = Vec::new();
        std::mem::swap(&mut self.blurred, &mut blurred);
        for identifier in blurred {
            let Some(chunk) = self.chunks.get(&identifier) else {
                self.remove_from_blurred(&identifier);
                continue;
            };
            let cell = chunk_index_cell(&chunk.centre(), self.grid_size());
            for (x, z) in neighbourhood(cell) {
                if let Some(column) = self.focus.get_mut(x, z) {
                    column.shut_if_seen(&identifier);
                }
            }
            self.focus_chunk_into_columns(&identifier);
            for (x, z) in neighbourhood(cell) {
                if let Some(column) = self.focus.get_mut(x, z) {
                    column.open_and_see(&identifier);
                }
            }
        }
    }

    /// `EdgeGeometryMapping::postBoundChunk`'s focus step
    /// (`chunk_loading/edge_geometry_mapping.cpp:213-216`): focus the
    /// chunk's hull right now — remove it from the blurred queue, insert
    /// the hull into the columns of its cell set and mark the nine columns
    /// around its centre as having seen it. The sync-loader face the port
    /// exposes so a caller that drives its own loader need not wait for the
    /// pump; [`ChunkSpace::focus`] runs the same three steps per queued
    /// chunk.
    pub fn focus_chunk(&mut self, identifier: &str) {
        self.remove_from_blurred(identifier);
        self.focus_chunk_into_columns(identifier);
        self.see_chunk(identifier);
    }

    /// `ServerChunkSpace::seeChunk` (`server_chunk_space.cpp:24-44`): record
    /// a chunk as seen in the nine columns around its centre.
    pub fn see_chunk(&mut self, identifier: &str) {
        let Some(chunk) = self.chunks.get(identifier) else {
            return;
        };
        let cell = chunk_index_cell(&chunk.centre(), self.grid_size());
        for (x, z) in neighbourhood(cell) {
            if let Some(column) = self.focus.get_mut(x, z) {
                column.open_and_see(identifier);
            }
        }
    }

    /// `ChunkSpace::noticeChunk` (`chunk_space.cpp:1359-1372`): tell the
    /// space that a chunk is now bound and may be focussed. The reference
    /// does nothing but hand the chunk to `blurredChunk`, which carries the
    /// bound test (`base_chunk_space.cpp:720-728`), so the guard stays there.
    fn notice_chunk(&mut self, identifier: &str) {
        self.blur_chunk(identifier);
    }

    /// `BaseChunkSpace::blurredChunk` (`base_chunk_space.cpp:720-728`): the
    /// bound chunk joins the blurred queue.
    fn blur_chunk(&mut self, identifier: &str) {
        if let Some(chunk) = self.chunks.get(identifier)
            && chunk.is_bound
            && !self.blurred.iter().any(|id| id == identifier)
        {
            self.blurred.push(identifier.to_string());
        }
    }

    /// `BaseChunkSpace::removeFromBlurred` (`base_chunk_space.cpp:734-747`).
    fn remove_from_blurred(&mut self, identifier: &str) -> bool {
        if let Some(pos) = self.blurred.iter().position(|id| id == identifier) {
            self.blurred.remove(pos);
            true
        } else {
            false
        }
    }

    /// `ChunkSpace::ignoreChunk` (`chunk_space.cpp:1301-1347`): drop the
    /// chunk from the focus grid — the nine columns around its centre are
    /// deleted, which the reference smudges into the blurred queue — then
    /// remove it from the map. A chunk that is not bound is refused, as the
    /// reference errors out and returns.
    pub fn ignore_chunk(&mut self, identifier: &str) -> bool {
        let (bound, cell) = match self.chunks.get(identifier) {
            Some(chunk) => (
                chunk.is_bound,
                chunk_index_cell(&chunk.centre(), self.grid_size()),
            ),
            None => return false,
        };
        if !bound {
            return false;
        }
        let mut reblur = Vec::new();
        for (x, z) in neighbourhood(cell) {
            if self.focus.in_span(x, z)
                && let Some(column) = self.focus.get_mut(x, z)
            {
                // the column is deleted, not marked stale: the reference
                // deletes it so that nothing ever finds this chunk in the
                // focus grid again (a query returning a dead chunk and an
                // item being added to it would be very bad)
                reblur.extend(column.holdings().iter().cloned());
                let _ = self.focus.set(x, z, None);
            }
        }
        for id in reblur {
            self.blur_chunk(&id);
        }
        self.remove_from_blurred(identifier);
        self.remove_chunk(identifier);
        true
    }

    /// `ChunkManager::scan`'s candidate collection (`chunk_manager.cpp:2132-2222`):
    /// walk the grid offsets around the camera cell nearest-first, take the
    /// cells inside the mapping's world bounds and within `max_load_path`
    /// world units, create their outside chunks, and return the ones not yet
    /// loaded. The reference creates the chunk and registers it in the map
    /// even when it is already loaded; only the not-yet-loaded ones are
    /// returned as worth loading.
    pub fn scan(&mut self, camera: &Vector3, max_load_path: f32) -> Vec<String> {
        let cam_x = self.point_to_grid(camera.x);
        let cam_z = self.point_to_grid(camera.z);
        let max_grid = self.point_to_grid(max_load_path).abs();
        // The offsets' world distance: grid * gridSize (gridToPoint).
        let grid_size = self.grid_size();
        let radius2 = move |i: i32, j: i32| {
            let x = i as f32 * grid_size;
            let z = j as f32 * grid_size;
            x * x + z * z
        };

        let mut offsets: Vec<(i32, i32)> = Vec::new();
        for i in -max_grid..=max_grid {
            for j in -max_grid..=max_grid {
                offsets.push((i, j));
            }
        }
        // `GridComp` (`chunk_manager.cpp:2116-2123`) sorts by squared radius;
        // the tie-break on the offsets makes the order deterministic where
        // the reference `std::sort` is not.
        offsets.sort_by(|a, b| {
            radius2(a.0, a.1)
                .partial_cmp(&radius2(b.0, b.1))
                .unwrap()
                .then(a.0.cmp(&b.0))
                .then(a.1.cmp(&b.1))
        });

        let mut worthy = Vec::new();
        for (i, j) in offsets {
            if radius2(i, j) > max_load_path * max_load_path {
                break;
            }
            let x = cam_x + i;
            let z = cam_z + j;
            for mapping in &self.mappings {
                if !mapping.in_world_bounds(x, z) {
                    continue;
                }
                // `gridToLocal` is the identity here (BW-10-02+ for the
                // transformed-mapping case).
                let identifier = mapping.outside_chunk_identifier(x, z, false);
                if !self.chunks.contains_key(&identifier) {
                    let chunk = Chunk::new(&identifier, mapping, &Matrix::identity());
                    self.chunks.insert(identifier.clone(), chunk);
                }
                if let Some(chunk) = self.chunks.get(&identifier)
                    && !chunk.is_loaded
                {
                    worthy.push(identifier);
                    if worthy.len() >= MAX_WORTHY_CHUNKS {
                        return worthy;
                    }
                }
            }
        }
        worthy
    }

    /// Load a chunk from its `.chunk` file via `resource_resolver`, creating
    /// the `Chunk` if it is not in the map yet (`findOrAddChunk`,
    /// `chunk_manager.cpp:2203-2210`). Returns whether the load succeeded.
    pub fn load_chunk(
        &mut self,
        identifier: &str,
        resource_resolver: &dyn Fn(&str) -> Option<DataSection>,
    ) -> bool {
        let file = format!("{identifier}.chunk");
        let Some(section) = resource_resolver(&file) else {
            return false;
        };
        let mapping = self.mappings[0].clone();
        let chunk = self
            .chunks
            .entry(identifier.to_string())
            .or_insert_with(|| Chunk::new(identifier, &mapping, &Matrix::identity()));
        chunk.load(&section, &Matrix::identity(), resource_resolver)
    }

    /// `Chunk::bind` / `bindPortals` server slice (`chunk.cpp:1039-1288`):
    /// bind a loaded chunk's unbound portals. Heaven portals bind
    /// immediately; a portal to a real chunk binds when that chunk is already
    /// bound, connecting back into the neighbour's matching portal
    /// (`Chunk::bind(Chunk*)`). Extern, invasive and empty portals stay
    /// unbound (their resolution needs the space column, BW-10-02). Every
    /// portal gets its world-space centre from the chunk transform first
    /// (`chunk.cpp:3339`). Returns false when the chunk is not loaded.
    pub fn bind(&mut self, identifier: &str) -> bool {
        let Some(chunk) = self.chunks.get(identifier) else {
            return false;
        };
        if !chunk.is_loaded {
            return false;
        }
        if chunk.is_bound {
            return true;
        }

        // Decide immutably which portals bind now: heaven always, a real
        // chunk target when that neighbour is already bound.
        let mut heaven: Vec<(usize, usize)> = Vec::new();
        let mut connect: Vec<(usize, usize, String)> = Vec::new();
        for &ji in &chunk.joints {
            for (pi, portal) in chunk.boundaries[ji].unbound_portals.iter().enumerate() {
                match &portal.target {
                    PortalTarget::Heaven => heaven.push((ji, pi)),
                    PortalTarget::Chunk(name)
                        if self.chunks.get(name).is_some_and(|c| c.is_bound) =>
                    {
                        connect.push((ji, pi, name.clone()));
                    }
                    _ => {}
                }
            }
        }

        let chunk = self.chunks.get_mut(identifier).unwrap();
        // World-space centres for every portal (bound or not).
        for boundary in &mut chunk.boundaries {
            for portal in boundary
                .bound_portals
                .iter_mut()
                .chain(boundary.unbound_portals.iter_mut())
            {
                portal.centre = chunk.transform.apply_point(&portal.lcentre);
            }
        }
        // Move the decided portals from unbound to bound (`bindPortal`).
        for (ji, pis) in group_by_joint(heaven, connect) {
            let boundary = &mut chunk.boundaries[ji];
            let unbound = std::mem::take(&mut boundary.unbound_portals);
            let (move_first, rest): (Vec<_>, Vec<_>) =
                unbound.into_iter().partition(|p| match &p.target {
                    PortalTarget::Heaven => true,
                    PortalTarget::Chunk(name) => pis.contains(name),
                    _ => false,
                });
            boundary.bound_portals.extend(move_first);
            boundary.unbound_portals = rest;
        }

        let chunk = self.chunks.get_mut(identifier).unwrap();
        chunk.is_bound = true;
        self.bind_order.push(identifier.to_string());
        // `Chunk::bind` tells the space it may be focussed now
        // (`chunk.cpp:1072`): the chunk joins the blurred queue.
        self.notice_chunk(identifier);

        // Two-way: each neighbour whose portal we bound gets its matching
        // portal bound too (`Chunk::bind(Chunk*)`, chunk.cpp:1741-1774).
        let requester = identifier.to_string();
        let back_links: Vec<String> = {
            let chunk = self.chunks.get(&requester).unwrap();
            let mut names = Vec::new();
            for boundary in &chunk.boundaries {
                for portal in &boundary.bound_portals {
                    if let PortalTarget::Chunk(name) = &portal.target
                        && self.chunks.get(name).is_some_and(|c| c.is_bound)
                    {
                        names.push(name.clone());
                    }
                }
            }
            names
        };
        for name in back_links {
            self.bind_back(&name, &requester);
        }
        true
    }

    /// `Chunk::bind(Chunk*)` (`chunk.cpp:1741-1774`): move the first unbound
    /// portal in `owner` that points back at `requester` to the bound list.
    fn bind_back(&mut self, owner: &str, requester: &str) {
        let Some(chunk) = self.chunks.get(owner) else {
            return;
        };
        let mut found = None;
        for (ji, boundary) in chunk.boundaries.iter().enumerate() {
            for (pi, portal) in boundary.unbound_portals.iter().enumerate() {
                if portal.target == PortalTarget::Chunk(requester.to_string()) {
                    found = Some((ji, pi));
                    break;
                }
            }
            if found.is_some() {
                break;
            }
        }
        let Some((ji, pi)) = found else { return };
        let chunk = self.chunks.get_mut(owner).unwrap();
        let boundary = &mut chunk.boundaries[ji];
        let unbound = std::mem::take(&mut boundary.unbound_portals);
        let mut kept = Vec::new();
        for (i, portal) in unbound.into_iter().enumerate() {
            if i == pi {
                boundary.bound_portals.push(portal);
            } else {
                kept.push(portal);
            }
        }
        boundary.unbound_portals = kept;
    }

    /// `ChunkSpace::findChunkFromPoint` (`chunk_space.cpp:731-751`): the
    /// focus column of the point's grid square, then the column's hull walk
    /// (`BaseChunkSpace::Column::findChunk`, `base_chunk_space.cpp:570-588`)
    /// — the outside chunk of the cell, displaced by any smaller-volume
    /// inside chunk whose hull contains the point. A cell outside the focus
    /// grid or a column that never took a chunk answer nothing: the query
    /// returns `None`, which is how the index answers a void cell.
    pub fn find_chunk_from_point(&self, point: &Vector3) -> Option<&Chunk> {
        let mut pt = *point;
        // a small fudge on point.y so points sitting exactly on a chunk
        // face still resolve (the reference's comment at
        // `chunk_space.cpp:733-737`)
        pt.y += 0.0001;
        let column = self.column_at(&pt)?;
        let id = column.find_chunk(&pt, &self.chunks)?;
        self.chunks.get(&id)
    }

    /// `ChunkSpace::findOutsideChunkFromPoint` (`chunk_space.cpp:712-724`):
    /// the outside chunk of the point's grid square, with no hull walk.
    pub fn find_outside_chunk_from_point(&self, point: &Vector3) -> Option<&Chunk> {
        let mut pt = *point;
        pt.y += 0.0001;
        let column = self.column_at(&pt)?;
        let id = column.outside_chunk()?.to_string();
        self.chunks.get(&id)
    }

    /// `ChunkSpace::findChunkFromPointExact` (`chunk_space.cpp:757-770`):
    /// [`find_chunk_from_point`] plus the `completed()` check — a chunk that
    /// has not finished loading is not a valid drop target. The port gates on
    /// the load flag: the reference's `completed()` is the focus count, and
    /// this crate's focus pump sets it for every chunk it registers, so the
    /// load flag is the port's stand-in for the reference's `updateCompleted`
    /// (which folds terrain and cache completion that have no port faces).
    pub fn find_chunk_from_point_exact(&self, point: &Vector3) -> Option<&Chunk> {
        match self.find_chunk_from_point(point) {
            Some(chunk) if chunk.is_loaded => Some(chunk),
            _ => None,
        }
    }

    /// Drop a chunk from the map (`ChunkSpace::removeChunk`). The caller is
    /// responsible for taking it out of the focus grid first
    /// ([`ChunkSpace::ignore_chunk`], the reference's `ignoreChunk`).
    pub fn remove_chunk(&mut self, identifier: &str) {
        self.chunks.remove(identifier);
    }

    /// Load and bind every candidate in scan order, returning the bind
    /// sequence (the sync form of `checkLoadingChunks`' bind-in-finish-order,
    /// `chunk_manager.cpp:1862-1894`: a single-threaded loader finishes the
    /// chunks in the order it started them). The batch ends with the focus
    /// pump: the reference runs `ChunkSpace::focus` from the camera update
    /// after the loader reports the newly bound chunks
    /// (`chunk_manager.cpp:281`).
    pub fn load_and_bind(
        &mut self,
        candidates: &[String],
        resource_resolver: &dyn Fn(&str) -> Option<DataSection>,
    ) -> Vec<String> {
        let mut bound = Vec::new();
        for identifier in candidates {
            if self.load_chunk(identifier, resource_resolver) {
                self.bind(identifier);
                bound.push(identifier.clone());
            }
        }
        self.focus();
        bound
    }
}

/// The hull border a chunk puts into the focus columns. `Chunk::focus`
/// builds it from the boundary planes `bounds_` names (local, then carried
/// to world by the chunk transform, `chunk.cpp:1547-1553`). A chunk this
/// crate constructed without loading has no formed boundaries — the editor
/// world helpers build chunks straight from `Chunk::new` — and for those the
/// border falls back to the six planes of its bounding box, which the port
/// already keeps in world space. For outside chunks that is exactly the
/// reference's synthesized grid faces.
fn chunk_index_border(chunk: &Chunk) -> Vec<PlaneEq> {
    if chunk.bounds.is_empty() {
        let bb = &chunk.bounding_box;
        return vec![
            PlaneEq::new(Vector3::new(1.0, 0.0, 0.0), bb.min.x),
            PlaneEq::new(Vector3::new(-1.0, 0.0, 0.0), -bb.max.x),
            PlaneEq::new(Vector3::new(0.0, 1.0, 0.0), bb.min.y),
            PlaneEq::new(Vector3::new(0.0, -1.0, 0.0), -bb.max.y),
            PlaneEq::new(Vector3::new(0.0, 0.0, 1.0), bb.min.z),
            PlaneEq::new(Vector3::new(0.0, 0.0, -1.0), -bb.max.z),
        ];
    }
    chunk.focus_border()
}

/// The grid cell a point falls into, in the form `seeChunk`, `focus` and
/// `ignoreChunk` use it: `int(cen.x / gridSize())` with the negative-value
/// decrement the reference repeats in each of the three call sites
/// (`server_chunk_space.cpp:28-31`, `chunk_space.cpp:1321-1324`). This
/// truncates rather than floors, so a negative centre sits in the cell the
/// reference's truncation puts it in.
fn chunk_index_cell(centre: &Vector3, grid_size: f32) -> (i32, i32) {
    let mut nx = (centre.x / grid_size) as i32;
    if centre.x < 0.0 {
        nx -= 1;
    }
    let mut nz = (centre.z / grid_size) as i32;
    if centre.z < 0.0 {
        nz -= 1;
    }
    (nx, nz)
}

/// The nine cells around a centre cell, `for (int x = nx-1; x <= nx+1; x++)`
/// over the same range in z.
fn neighbourhood(cell: (i32, i32)) -> [(i32, i32); 9] {
    let (nx, nz) = cell;
    let mut out = [(0, 0); 9];
    let mut i = 0;
    for x in (nx - 1)..=(nx + 1) {
        for z in (nz - 1)..=(nz + 1) {
            out[i] = (x, z);
            i += 1;
        }
    }
    out
}

/// Group the `(joint, portal, neighbour)` connections and the heaven decisions
/// by joint index, returning `(joint, set of neighbour names to bind)`.
fn group_by_joint(
    heaven: Vec<(usize, usize)>,
    connect: Vec<(usize, usize, String)>,
) -> Vec<(usize, Vec<String>)> {
    let mut groups: Vec<(usize, Vec<String>)> = Vec::new();
    for (ji, _pi) in heaven {
        if !groups.iter().any(|(j, _)| *j == ji) {
            groups.push((ji, Vec::new()));
        }
    }
    for (ji, _pi, name) in connect {
        if let Some((_, names)) = groups.iter_mut().find(|(j, _)| *j == ji) {
            names.push(name);
        } else {
            groups.push((ji, vec![name]));
        }
    }
    groups
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::boundary::{ChunkBoundary, Portal};
    use crate::mapping::{GeometryMapping, GridBounds};
    use foundation::math::{BoundBox, PlaneEq};

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

    fn face_portal(target: &str) -> Portal {
        Portal::from_plane_points(
            PlaneEq::new(Vector3::new(1.0, 0.0, 0.0), 0.0),
            Vector3::new(0.0, 1.0, 0.0),
            &[
                Vector3::new(-10.0, -10.0, 0.0),
                Vector3::new(10.0, -10.0, 0.0),
                Vector3::new(10.0, 10.0, 0.0),
                Vector3::new(-10.0, 10.0, 0.0),
            ],
            target,
        )
    }

    /// A boundary holding several unbound portals to the same neighbour.
    fn two_portal_boundary() -> ChunkBoundary {
        ChunkBoundary {
            plane: PlaneEq::new(Vector3::new(1.0, 0.0, 0.0), 0.0),
            bound_portals: Vec::new(),
            unbound_portals: vec![face_portal("00000000o"), face_portal("00000000o")],
        }
    }

    /// `Chunk::bind(Chunk*)` moves only the *first* matching portal and
    /// returns (`chunk.cpp:1741-1774`); the second stays unbound.
    #[test]
    fn bind_back_moves_only_the_first_matching_portal() {
        let mut space = ChunkSpace::new(0, vec![identity_mapping()]);
        let mut owner = Chunk::new("00000001o", &identity_mapping(), &Matrix::identity());
        owner.boundaries = vec![two_portal_boundary()];
        owner.joints = vec![0];
        owner.is_loaded = true;
        space.add_chunk(owner);

        space.bind_back("00000001o", "00000000o");
        let boundary = &space.chunk("00000001o").unwrap().boundaries[0];
        assert_eq!(boundary.bound_portals.len(), 1);
        assert_eq!(boundary.unbound_portals.len(), 1);
        assert_eq!(
            boundary.bound_portals[0].target,
            PortalTarget::Chunk("00000000o".into())
        );

        // A missing owner is a no-op.
        space.bind_back("no-such-chunk", "00000000o");
        assert_eq!(
            space.chunk("00000001o").unwrap().boundaries[0]
                .bound_portals
                .len(),
            1
        );
    }

    /// A joint that holds both a binding decision and an unresolvable portal
    /// keeps the latter unbound while the former moves over (`bindPortal`).
    #[test]
    fn bind_keeps_special_portals_unbound_in_a_mixed_joint() {
        let mut space = ChunkSpace::new(0, vec![identity_mapping()]);
        let mut chunk = Chunk::new("00000000o", &identity_mapping(), &Matrix::identity());
        chunk.boundaries = vec![ChunkBoundary {
            plane: PlaneEq::new(Vector3::new(1.0, 0.0, 0.0), 0.0),
            bound_portals: Vec::new(),
            unbound_portals: vec![face_portal("heaven"), face_portal("extern")],
        }];
        chunk.joints = vec![0];
        chunk.is_loaded = true;
        space.add_chunk(chunk);

        assert!(space.bind("00000000o"));
        let boundary = &space.chunk("00000000o").unwrap().boundaries[0];
        assert_eq!(
            boundary
                .bound_portals
                .iter()
                .map(|p| p.target.clone())
                .collect::<Vec<_>>(),
            vec![PortalTarget::Heaven]
        );
        assert_eq!(
            boundary
                .unbound_portals
                .iter()
                .map(|p| p.target.clone())
                .collect::<Vec<_>>(),
            vec![PortalTarget::Extern]
        );
    }

    /// A scan stops at `MAX_WORTHY_CHUNKS` candidates even when the load
    /// radius covers more cells (`chunk_manager.cpp:2215`).
    #[test]
    fn scan_caps_the_candidates_at_max_worthy_chunks() {
        let big = GeometryMapping::new(
            "space/",
            100.0,
            GridBounds {
                min_l_grid_x: -9,
                min_l_grid_y: -9,
                max_l_grid_x: 9,
                max_l_grid_y: 9,
                min_grid_x: -9,
                min_grid_y: -9,
                max_grid_x: 9,
                max_grid_y: 9,
            },
            false,
        );
        let mut space = ChunkSpace::new(0, vec![big]);
        let candidates = space.scan(&Vector3::ZERO, 1000.0);
        assert_eq!(candidates.len(), MAX_WORTHY_CHUNKS);
        // Each candidate names a distinct chunk.
        let mut dedup = candidates.clone();
        dedup.sort();
        dedup.dedup();
        assert_eq!(dedup.len(), MAX_WORTHY_CHUNKS);
    }

    #[test]
    fn group_by_joint_merges_heaven_and_connections_per_joint() {
        // A heaven-only joint gets an empty connection list...
        assert_eq!(
            group_by_joint(vec![(2, 0)], Vec::new()),
            vec![(2, Vec::new())]
        );
        // ...and a joint that has both merges the name into the same group.
        assert_eq!(
            group_by_joint(vec![(1, 0)], vec![(1, 0, "00010000o".to_string())]),
            vec![(1, vec!["00010000o".to_string()])]
        );
        // A connection without a heaven decision starts its own group.
        assert_eq!(
            group_by_joint(Vec::new(), vec![(4, 1, "00000000i".to_string())]),
            vec![(4, vec!["00000000i".to_string()])]
        );
    }
    #[test]
    fn group_by_joint_dedupes_a_repeated_heaven_decision() {
        // Two heaven decisions on one joint open a single group.
        assert_eq!(
            group_by_joint(vec![(3, 0), (3, 1)], Vec::new()),
            vec![(3, Vec::new())]
        );
        // A repeated heaven decision does not displace the connections already
        // gathered for that joint.
        assert_eq!(
            group_by_joint(vec![(3, 0), (3, 1)], vec![(3, 0, "00010000o".to_string())]),
            vec![(3, vec!["00010000o".to_string()])]
        );
    }

    #[test]
    fn inside_chunks_take_the_column_by_smallest_volume() {
        // no outside chunk is loaded: the smallest containing inside chunk
        // wins the column, and a larger one never displaces it
        let mapping = identity_mapping();
        let mut space = ChunkSpace::new(0, vec![mapping]);
        for (id, volume) in [
            ("a0000000i", 8000.0_f32),
            ("b0000000i", 1000.0),
            ("c0000000i", 4000.0),
        ] {
            let mut chunk = Chunk::new(id, &identity_mapping(), &Matrix::identity());
            let edge = volume.cbrt();
            chunk.bounding_box = BoundBox::new(
                Vector3::new(50.0 - edge / 2.0, -edge / 2.0, 50.0 - edge / 2.0),
                Vector3::new(50.0 + edge / 2.0, edge / 2.0, 50.0 + edge / 2.0),
            );
            chunk.is_loaded = true;
            space.add_chunk(chunk);
        }
        let winner = space
            .find_chunk_from_point_exact(&Vector3::new(50.0, 0.0, 50.0))
            .map(|c| c.identifier.clone());
        assert_eq!(winner.as_deref(), Some("b0000000i"));
    }
}
