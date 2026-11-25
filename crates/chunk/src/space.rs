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
//! Registered divergences (see `docs/review/bw10-01-chunk-load.md`): the
//! within-equal-radius order of the reference `std::sort` is unspecified, so
//! this port pins a deterministic tie-break (radius, then offset x, then z)
//! and the tests assert the radius order and cell set, not a brittle per-tier
//! sequence; `resolveExtern`/invasive portal resolution need the space column
//! (`guessChunk`) and `ChunkOverlappers` belong to the scene-adaptation
//! surface (BW-10-02); `gridToLocal` is the identity (transformed mappings
//! are BW-10-02+). The unload slice of the same `scan` plus `Chunk::unbind`
//! and `Chunk::unload` (BW-10-01-04) run as the separate `unload_farthest`
//! face: the reference loads and unloads in one `scan` pass, the port keeps
//! the two candidate passes synchronous and separate.
//!
//! BW-10-01-05 layers the spatial index on top: the focus grid of
//! [`Column`](crate::Column)s, the blurred list and the focus pump
//! (`ChunkSpace::focus`, `ServerChunkSpace::focus` at
//! `server_chunk_space.cpp:38-93`), the column point queries
//! (`findChunkFromPoint`/`findChunkFromPointExact`,
//! `chunk_space.cpp:736-770`) and the unload/notice pair
//! (`ignoreChunk`/`noticeChunk`, `chunk_space.cpp:1300-1375`) that keeps the
//! grid honest as chunks bind and go away.

use std::collections::BTreeMap;

use foundation::math::{BoundBox, Matrix, Vector3};
use resources::datasection::DataSection;

use crate::boundary::PortalTarget;
use crate::chunk::Chunk;
use crate::column::{Column, ColumnGrid};
use crate::mapping::GeometryMapping;

/// `MAX_WORTHY_CHUNKS` (`chunk_manager.cpp:70`): a scan visits at most this
/// many not-yet-loaded chunks.
const MAX_WORTHY_CHUNKS: usize = 50;

/// `maxUnloadChunks_` for the non-editor server build
/// (`chunk_manager.cpp:92-95`): one chunk per unload pass.
const MAX_UNLOAD_CHUNKS: usize = 1;

/// `CAMERA_NEARBY_WIRED_BIAS` (`chunk_manager.cpp:2085`): a chunk whose
/// bounds reach within this of the camera grid corner stays loaded.
const CAMERA_NEARBY_WIRED_BIAS: f32 = 20.0;

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
    /// `currentFocus_`: the grid window of columns the index serves
    /// (`ServerChunkSpace::ColumnGrid`, `server_chunk_space.hpp:34-90`).
    columns: ColumnGrid,
    /// `blurred_`: the bound chunks waiting for the next focus pump
    /// (`BaseChunkSpace::blurredChunk`, `base_chunk_space.cpp:720-730`).
    blurred: Vec<String>,
}

impl ChunkSpace {
    pub fn new(id: u16, mappings: Vec<GeometryMapping>) -> Self {
        let mut space = Self {
            id,
            mappings,
            chunks: BTreeMap::new(),
            bind_order: Vec::new(),
            columns: ColumnGrid::new(),
            blurred: Vec::new(),
        };
        // `ChunkSpace::ChunkSpace` ends in `recalcGridBounds`
        // (`chunk_space.cpp:230`).
        space.recalc_grid_bounds();
        space
    }

    /// The space's grid size (`ChunkSpace::gridSize`), from its mapping.
    pub fn grid_size(&self) -> f32 {
        self.mappings[0].grid_size
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
    pub fn add_chunk(&mut self, chunk: Chunk) {
        self.chunks.insert(chunk.identifier.clone(), chunk);
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

        // `pSpace_->noticeChunk(this)` (`chunk.cpp:1082`): the space can
        // focus the chunk now. The overlapped-outside-chunk notifications
        // that follow for an inside chunk belong to the scene-adaptation
        // face (BW-10-02) and are registered.
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

    /// Load and bind every candidate in scan order, returning the bind
    /// sequence (the sync form of `checkLoadingChunks`' bind-in-finish-order,
    /// `chunk_manager.cpp:1862-1894`: a single-threaded loader finishes the
    /// chunks in the order it started them).
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
        bound
    }

    /// `Chunk::unbind( bool cut )` (`chunk.cpp:1292-1458`): undo the chunk's
    /// side of every binding. A heaven portal of an inside chunk unbinds on
    /// its own; every other non-chunk target — heaven of an outside chunk,
    /// earth, extern, invasive — stays bound, the reference's
    /// `!p.hasChunk()` continue. A chunk connection unbinds both ways: under
    /// `cut` the target becomes INVASIVE when this chunk is inside and the
    /// neighbour outside, NOTHING otherwise, and the neighbour undoes its
    /// reverse link, forced to cut when this chunk is outside and the
    /// neighbour inside. Returns false when the chunk is not in the map.
    pub fn unbind(&mut self, identifier: &str, cut: bool) -> bool {
        if !self.chunks.contains_key(identifier) {
            return false;
        }
        let is_outside = self.chunks[identifier].is_outside;
        // The walk reads `joints`; the neighbour legs never touch this
        // chunk's own lists, so the snapshot is the joint order.
        let joints = self.chunks[identifier].joints.clone();
        for ji in joints {
            // The live index: removals shift the bound list left, so only
            // an advance past a surviving portal moves `i`.
            let mut i = 0usize;
            loop {
                // What this bound portal does: release (inside heaven),
                // stay (every non-chunk target), or connect (a chunk).
                enum Step {
                    Release,
                    Stay,
                    Connect(String),
                }
                let step = self
                    .chunks
                    .get(identifier)
                    .and_then(|c| c.boundaries[ji].bound_portals.get(i))
                    .map(|portal| match &portal.target {
                        // A heaven portal of an inside chunk unbinds itself;
                        // heaven of an outside chunk falls through to Stay.
                        PortalTarget::Heaven if !is_outside => Step::Release,
                        PortalTarget::Chunk(name) => Step::Connect(name.clone()),
                        _ => Step::Stay,
                    });
                match step {
                    None => break,
                    Some(Step::Stay) => i += 1,
                    Some(Step::Release) => {
                        self.unbind_portal(identifier, ji, i);
                    }
                    Some(Step::Connect(name)) => {
                        let neighbour = self.chunks.get(&name).map(|c| c.is_outside);
                        if cut {
                            // `!isOutsideChunk() && p.pChunk->isOutsideChunk()`
                            // keeps the shell hint, everything else drops.
                            let next = match neighbour {
                                Some(true) if !is_outside => PortalTarget::Invasive,
                                _ => PortalTarget::Nothing,
                            };
                            self.chunks.get_mut(identifier).unwrap().boundaries[ji].bound_portals
                                [i]
                                .target = next;
                        }
                        self.unbind_portal(identifier, ji, i);
                        // The neighbour undoes its reverse link; an outside
                        // chunk always cuts an inside neighbour.
                        let effective = match (is_outside, neighbour) {
                            (true, Some(false)) => true,
                            _ => cut,
                        };
                        self.unbind_back(&name, identifier, effective);
                    }
                }
            }
        }
        self.chunks.get_mut(identifier).unwrap().is_bound = false;
        true
    }

    /// `ChunkBoundary::unbindPortal` (`chunk_boundary.cpp:185-191`): move
    /// the bound portal to the **back** of the unbound list — the position
    /// a later bind re-finds first.
    fn unbind_portal(&mut self, owner: &str, joint: usize, index: usize) {
        let chunk = self.chunks.get_mut(owner).unwrap();
        let boundary = &mut chunk.boundaries[joint];
        let portal = boundary.bound_portals.remove(index);
        boundary.unbound_portals.push(portal);
    }

    /// `Chunk::unbind( Chunk * pRequester, bool cut )`
    /// (`chunk.cpp:2083-2136`): undo the first bound portal in `owner` that
    /// points back at `requester`. Under `cut` the target becomes INVASIVE
    /// when the owner is inside and the requester outside, NOTHING
    /// otherwise; an internal portal instead loses its whole boundary from
    /// `joints`, its portal staying bound inside the orphaned boundary. A
    /// missing owner or no matching portal is a silent no-op (the reference
    /// ERRORs there; the links are symmetric through the public faces, the
    /// same precedent as `bind_back`'s missing owner).
    fn unbind_back(&mut self, owner: &str, requester: &str, cut: bool) {
        let Some(chunk) = self.chunks.get(owner) else {
            return;
        };
        let is_outside = chunk.is_outside;
        let requester_outside = self.chunks.get(requester).map(|c| c.is_outside);
        let mut found = None;
        'outer: for &ji in &chunk.joints {
            for (pi, portal) in chunk.boundaries[ji].bound_portals.iter().enumerate() {
                if portal.target == PortalTarget::Chunk(requester.to_string()) {
                    found = Some((ji, pi));
                    break 'outer;
                }
            }
        }
        let Some((ji, pi)) = found else {
            return;
        };
        if cut {
            let next = match requester_outside {
                Some(true) if !is_outside => PortalTarget::Invasive,
                _ => PortalTarget::Nothing,
            };
            let internal = self.chunks[owner].boundaries[ji].bound_portals[pi].internal;
            if internal {
                // `joints_.erase( bit )` (`chunk.cpp:2116-2127`): the
                // boundary leaves the joints and the walk returns without
                // unbinding the portal.
                self.chunks
                    .get_mut(owner)
                    .unwrap()
                    .joints
                    .retain(|&j| j != ji);
                return;
            }
            self.chunks.get_mut(owner).unwrap().boundaries[ji].bound_portals[pi].target = next;
        }
        self.unbind_portal(owner, ji, pi);
    }

    /// `Chunk::unload` (`chunk.cpp:914-984`): release the chunk's scene
    /// objects and boundary indices. A still-bound chunk refuses (the
    /// reference ERRORs); a chunk that is not loaded has nothing to
    /// release. The boundaries themselves stay — the chunk remains
    /// appointed by its chunk space, and a reload re-forms the boundary
    /// indices (`form_boundaries`).
    pub fn unload_chunk(&mut self, identifier: &str) -> bool {
        let Some(chunk) = self.chunks.get(identifier) else {
            return false;
        };
        if chunk.is_bound {
            return false;
        }
        if !chunk.is_loaded {
            return false;
        }
        // `focusCount_ = 0; updateCompleted();` (`chunk.cpp:1336-1338`). The
        // reference's `ignoreChunk` call one line above refuses here too:
        // the unload path always unbinds first, and `ignoreChunk` refuses an
        // unbound chunk (`chunk_space.cpp:1307-1313`), so the hull stays in
        // its column — `findChunkFromPointExact`'s `completed()` check is
        // what keeps a query off it.
        let chunk = self.chunks.get_mut(identifier).unwrap();
        chunk.focus_count = 0;
        chunk.items.clear();
        chunk.bounds.clear();
        chunk.joints.clear();
        chunk.is_loaded = false;
        true
    }

    /// `ChunkManager::scan`'s unload slice (`chunk_manager.cpp:2236-2366`):
    /// take at most `MAX_UNLOAD_CHUNKS` bound, removable chunks that are
    /// far enough from the camera grid corner, not covered by a loaded
    /// outside chunk, and outside the camera's nearby bias; unbind and
    /// unload each. The chunk **stays in the map** — it is still appointed
    /// by its chunk space, so a later load finds it without re-creating.
    /// Returns the unloaded identifiers in the order they were picked.
    pub fn unload_farthest(&mut self, camera: &Vector3, min_unload_path: f32) -> Vec<String> {
        // The camera grid corner (`chunk_manager.cpp:2226-2233`): the corner
        // of the camera's grid cell, not the camera point itself.
        let camera_grid = Vector3::new(
            self.grid_to_point(self.point_to_grid(camera.x)),
            0.0,
            self.grid_to_point(self.point_to_grid(camera.z)),
        );
        let identifiers: Vec<String> = self.chunks.keys().cloned().collect();
        let mut furthest = Vec::new();
        for identifier in identifiers {
            if furthest.len() >= MAX_UNLOAD_CHUNKS {
                break;
            }
            let chunk = &self.chunks[&identifier];
            if !chunk.is_bound || !chunk.removable {
                continue;
            }
            // Planar squared distance of the chunk origin against the
            // hysteresis path (`chunk_manager.cpp:2245-2252`).
            let origin = chunk.transform.apply_to_origin();
            let dx = origin.x - camera_grid.x;
            let dz = origin.z - camera_grid.z;
            if dx * dx + dz * dz <= min_unload_path * min_unload_path {
                continue;
            }
            if !chunk.is_outside {
                // An inside chunk whose covering outside chunk is still
                // loaded stays (`chunk_manager.cpp:2250-2256`).
                let covering = chunk.mapping.outside_chunk_identifier(
                    self.point_to_grid(origin.x),
                    self.point_to_grid(origin.z),
                    true,
                );
                if self.chunks.get(&covering).is_some_and(|c| c.is_loaded) {
                    continue;
                }
            }
            // A chunk whose bounds reach into the camera's nearby bias
            // stays (`chunk_manager.cpp:2262`).
            if intersects_with_bias(&chunk.bounding_box, &camera_grid, CAMERA_NEARBY_WIRED_BIAS) {
                continue;
            }
            furthest.push(identifier);
        }
        for identifier in &furthest {
            self.unbind(identifier, false);
            self.unload_chunk(identifier);
        }
        furthest
    }

    /// `ChunkSpace::recalcGridBounds` (`chunk_space.cpp:1077-1123`) plus
    /// `ServerChunkSpace::recalcGridBounds` (`server_chunk_space.cpp:132-137`)
    /// plus `ColumnGrid::rect` (`server_chunk_space.cpp:143-176`): the grid
    /// window is the mappings' world rect. Columns that leave the window are
    /// deleted there, which smudges the chunks they held — every dropped
    /// column's holdings go to the blurred list (`ignoreChunk`'s column
    /// deletion rule, `chunk_space.cpp:1315-1345`).
    pub fn recalc_grid_bounds(&mut self) {
        if self.mappings.is_empty() {
            self.columns.rect(0, 0, 0, 0, &mut Vec::new());
            return;
        }
        let min_x = self
            .mappings
            .iter()
            .map(|m| m.bounds.min_grid_x)
            .min()
            .unwrap();
        let min_z = self
            .mappings
            .iter()
            .map(|m| m.bounds.min_grid_y)
            .min()
            .unwrap();
        let max_x = self
            .mappings
            .iter()
            .map(|m| m.bounds.max_grid_x)
            .max()
            .unwrap();
        let max_z = self
            .mappings
            .iter()
            .map(|m| m.bounds.max_grid_y)
            .max()
            .unwrap();

        // `rect(minGridX_, minGridY_, maxGridX_-minGridX_+1, ...)` — the
        // window is a half-open rect, so the +1 covers the last cell.
        let mut dropped = Vec::new();
        self.columns.rect(
            min_x,
            min_z,
            max_x - min_x + 1,
            max_z - min_z + 1,
            &mut dropped,
        );
        // deleting a column smudges the chunks in it (adds to blurred list)
        for column in dropped {
            for holder in column.holdings() {
                self.blur(holder);
            }
        }
    }

    /// The focus-grid cell of a point (`ignoreChunk`/`focus`,
    /// `chunk_space.cpp:1310-1311` / `server_chunk_space.cpp:47-50`):
    /// `int(c / gridSize)`, stepping down on the negative side so a negative
    /// coordinate never floors toward zero.
    fn grid_cell(&self, x: f32, z: f32) -> (i32, i32) {
        let grid_size = self.grid_size();
        let mut nx = (x / grid_size) as i32;
        if x < 0.0 {
            nx -= 1;
        }
        let mut nz = (z / grid_size) as i32;
        if z < 0.0 {
            nz -= 1;
        }
        (nx, nz)
    }

    /// `ChunkSpace::findChunkFromPoint` (`chunk_space.cpp:729-750`): the
    /// chunk that owns the point, via the point's column. A tiny fudge on y
    /// keeps points sitting exactly on a chunk face inside it. The blank
    /// arm is the outside-chunk lookup `chunk_space.cpp:714-727` shares.
    pub fn find_chunk_from_point(&mut self, point: &Vector3) -> Option<&Chunk> {
        let mut pt = *point;
        pt.y += 0.0001;
        // `this->column(pt, false)`: an existing column or none
        let (x, z) = self.grid_cell(pt.x, pt.z);
        let column = self.columns.get(x, z)?;
        let winner = column.find_chunk(&pt, &self.chunks)?;
        self.chunks.get(&winner)
    }

    /// `ChunkSpace::findChunkFromPoint`'s outside-only arm
    /// (`chunk_space.cpp:714-727`): the column's outside chunk, without the
    /// hull walk.
    pub fn find_outside_chunk_from_point(&mut self, point: &Vector3) -> Option<&Chunk> {
        let mut pt = *point;
        pt.y += 0.0001;
        let (x, z) = self.grid_cell(pt.x, pt.z);
        let column = self.columns.get(x, z)?;
        let winner = column.outside_chunk()?.to_string();
        self.chunks.get(&winner)
    }

    /// `ChunkSpace::findChunkFromPointExact` (`chunk_space.cpp:753-770`):
    /// [`find_chunk_from_point`] plus the `completed()` check — an incomplete
    /// chunk is not a valid query answer.
    pub fn find_chunk_from_point_exact(&mut self, point: &Vector3) -> Option<&Chunk> {
        let winner = self.find_chunk_from_point(point)?;
        if winner.completed() {
            Some(winner)
        } else {
            None
        }
    }

    /// `BaseChunkSpace::blurredChunk` (`base_chunk_space.cpp:720-730`): park
    /// a bound chunk on the blurred list for the next focus pump. The
    /// reference can push a chunk twice when it blurs while already queued;
    /// the port's dedup drops the second entry — the pump is idempotent
    /// over the same chunk either way (registered divergence).
    fn blur(&mut self, identifier: &str) {
        if !self.chunks.get(identifier).is_some_and(|c| c.is_bound) {
            return;
        }
        if !self.blurred.iter().any(|id| id == identifier) {
            self.blurred.push(identifier.to_string());
        }
    }

    /// `ChunkSpace::noticeChunk` (`chunk_space.cpp:1359-1375`): a chunk that
    /// has just bound queues itself for the next focus pump. The MF_SERVER
    /// legs (visibility boxes, the quad tree) are client-only and registered.
    pub fn notice_chunk(&mut self, identifier: &str) {
        self.blur(identifier);
    }

    /// `ServerChunkSpace::focus` (`server_chunk_space.cpp:38-93`) plus
    /// `Chunk::focus` (`chunk.cpp:1536-1614`): pump the blurred list. A
    /// queued chunk shuts the 3×3 columns around its centre that have seen
    /// it (so a re-add can replace its hull), focuses — the outside chunk
    /// into the column of its centre, an inside chunk into the distinct
    /// columns of its bounding box corners — and the columns open and see it
    /// again. The cache focus loop (`chunk.cpp:1596-1601`) has no port-side
    /// face and is registered.
    pub fn focus(&mut self) {
        while let Some(identifier) = self.blurred.first().cloned() {
            self.blurred.remove(0);

            let (centre, border, is_outside, bounding_box) = {
                let Some(chunk) = self.chunks.get(&identifier) else {
                    continue;
                };
                (
                    chunk.centre(),
                    chunk.focus_border(),
                    chunk.is_outside,
                    chunk.bounding_box,
                )
            };

            // find out where it is in the focus grid
            let (nx, nz) = self.grid_cell(centre.x, centre.z);

            // see if this chunk is new to (nx,nz) and adjacent columns,
            // close them if it isn't
            for x in nx - 1..=nx + 1 {
                for z in nz - 1..=nz + 1 {
                    if let Some(column) = self.columns.get_mut(x, z) {
                        column.shut_if_seen(&identifier);
                    }
                }
            }

            // do the actual focussing work: outside chunks take the column
            // of their centre (which creates it), inside chunks the distinct
            // columns of their bounding box corners (`chunk.cpp:1560-1591`)
            let mut cell_set: Vec<(i32, i32)> = Vec::new();
            if is_outside {
                let cell = self.grid_cell(centre.x, centre.z);
                cell_set.push(cell);
            } else {
                let min = bounding_box.min;
                let max = bounding_box.max;
                for i in 0..8 {
                    let pt = Vector3::new(
                        if i & 1 != 0 { max.x } else { min.x },
                        if i & 2 != 0 { max.y } else { min.y },
                        if i & 4 != 0 { max.z } else { min.z },
                    );
                    let cell = self.grid_cell(pt.x, pt.z);
                    if !cell_set.contains(&cell) {
                        cell_set.push(cell);
                    }
                }
            }
            for (x, z) in cell_set {
                // create the column first (`ChunkSpace::column`'s create
                // leg), then add the hull — the column and the chunk are
                // disjoint fields of the space, so the two borrows coexist
                self.column_at(x, z, true);
                let Some(chunk) = self.chunks.get(&identifier) else {
                    continue;
                };
                if let Some(column) = self.columns.get_mut(x, z) {
                    column.add_chunk(&border, chunk);
                }
            }

            // `focusCount_ = 1` (`chunk.cpp:1605`)
            if let Some(chunk) = self.chunks.get_mut(&identifier) {
                chunk.focus_count = 1;
            }

            // open all the columns, and mark them as having seen this chunk
            for x in nx - 1..=nx + 1 {
                for z in nz - 1..=nz + 1 {
                    if let Some(column) = self.columns.get_mut(x, z) {
                        column.open_and_see(&identifier);
                    }
                }
            }
        }
    }

    /// The column at an already-known grid cell, created on request
    /// (`Chunk::focus` creates the centre column through
    /// `ChunkSpace::column`, `chunk.cpp:1561-1563`).
    fn column_at(&mut self, x: i32, z: i32, can_create: bool) -> Option<&mut Column> {
        if !self.columns.in_span(x, z) {
            return None;
        }
        if self.columns.get(x, z).is_none() && can_create {
            self.columns.set(x, z, Some(Column::new()));
        }
        self.columns.get_mut(x, z)
    }

    /// `ChunkSpace::ignoreChunk` (`chunk_space.cpp:1300-1355`): a chunk
    /// about to be disposed leaves the focus grid. The 3×3 columns around
    /// its centre are deleted outright — never a find in them again — and
    /// the deleted columns' holdings go to the blurred list (the smudge).
    /// An unbound chunk refuses. The MF_SERVER quad tree leg is client-only
    /// and registered.
    pub fn ignore_chunk(&mut self, identifier: &str) {
        // can't ignore unbound chunks
        if !self.chunks.get(identifier).is_some_and(|c| c.is_bound) {
            // ERROR_MSG "Attempted to ignore offline chunk"
            return;
        }

        let (centre_x, centre_z) = {
            let Some(chunk) = self.chunks.get(identifier) else {
                return;
            };
            let centre = chunk.centre();
            (centre.x, centre.z)
        };
        let (nx, nz) = self.grid_cell(centre_x, centre_z);

        // and get it out of there: the whole 3×3 block of columns goes,
        // because a hull can be sitting in any of them
        for x in nx - 1..=nx + 1 {
            for z in nz - 1..=nz + 1 {
                if self.columns.in_span(x, z)
                    && let Some(column) = self.columns.set(x, z, None)
                {
                    // deleting the column smudges chunks in it (adds to
                    // blurred list). Use this when recreating column.
                    for holder in column.holdings() {
                        self.blur(holder);
                    }
                }
            }
        }

        // the chunk itself leaves the blurred list
        self.blurred.retain(|id| id != identifier);
    }
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

/// `AABB::intersects( const Vector3 & v, float bias )`
/// (`boundbox.ipp:313-317`): per axis `v >= min - bias && v < max + bias` —
/// half-open at the inflated max. The port's `BoundBox` carries the box-box
/// form only, so the point-with-bias test the unload pass needs lives here.
fn intersects_with_bias(bb: &BoundBox, v: &Vector3, bias: f32) -> bool {
    v.x >= bb.min.x - bias
        && v.y >= bb.min.y - bias
        && v.z >= bb.min.z - bias
        && v.x < bb.max.x + bias
        && v.y < bb.max.y + bias
        && v.z < bb.max.z + bias
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::boundary::{ChunkBoundary, Portal};
    use crate::mapping::{GeometryMapping, GridBounds};
    use foundation::math::PlaneEq;

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

    /// `ChunkBoundary::unbindPortal` lands the portal at the **back** of the
    /// unbound list (`chunk_boundary.cpp:185-191`).
    #[test]
    fn unbind_portal_lands_at_the_back_of_the_unbound_list() {
        let mut space = ChunkSpace::new(0, vec![identity_mapping()]);
        let mut owner = Chunk::new("00000000o", &identity_mapping(), &Matrix::identity());
        owner.boundaries = vec![ChunkBoundary {
            plane: PlaneEq::new(Vector3::new(1.0, 0.0, 0.0), 0.0),
            bound_portals: vec![
                face_portal("00000001o"),
                face_portal("00000002o"),
                face_portal("00000003o"),
            ],
            unbound_portals: vec![face_portal("extern")],
        }];
        owner.joints = vec![0];
        space.add_chunk(owner);

        space.unbind_portal("00000000o", 0, 1);
        let boundary = &space.chunk("00000000o").unwrap().boundaries[0];
        assert_eq!(
            boundary
                .bound_portals
                .iter()
                .map(|p| p.target.clone())
                .collect::<Vec<_>>(),
            vec![
                PortalTarget::Chunk("00000001o".into()),
                PortalTarget::Chunk("00000003o".into())
            ]
        );
        // The removed portal joins *after* the pre-existing unbound one.
        assert_eq!(
            boundary
                .unbound_portals
                .iter()
                .map(|p| p.target.clone())
                .collect::<Vec<_>>(),
            vec![
                PortalTarget::Extern,
                PortalTarget::Chunk("00000002o".into())
            ]
        );
    }

    /// `Chunk::unbind(Chunk*, true)` on an internal portal erases the whole
    /// boundary from `joints` and returns without unbinding the portal
    /// (`chunk.cpp:2116-2127`); a normal portal under cut is rewritten to
    /// the flattened target and unbound.
    #[test]
    fn unbind_back_cuts_internal_and_normal_portals_differently() {
        let mut space = ChunkSpace::new(0, vec![identity_mapping()]);
        let mut owner = Chunk::new("00000001o", &identity_mapping(), &Matrix::identity());
        let mut internal = face_portal("00000002o");
        internal.internal = true;
        owner.boundaries = vec![
            ChunkBoundary {
                plane: PlaneEq::new(Vector3::new(1.0, 0.0, 0.0), 0.0),
                bound_portals: vec![internal],
                unbound_portals: Vec::new(),
            },
            ChunkBoundary {
                plane: PlaneEq::new(Vector3::new(-1.0, 0.0, 0.0), -100.0),
                bound_portals: vec![face_portal("00000002o")],
                unbound_portals: Vec::new(),
            },
        ];
        owner.joints = vec![0, 1];
        space.add_chunk(owner);

        // The internal leg: boundary 0 leaves the joints, its portal stays
        // bound inside the orphaned boundary.
        space.unbind_back("00000001o", "00000002o", true);
        let chunk = space.chunk("00000001o").unwrap();
        assert_eq!(chunk.joints, vec![1]);
        assert_eq!(chunk.boundaries[0].bound_portals.len(), 1);

        // The normal leg: boundary 1's portal is rewritten to NOTHING and
        // moves to the unbound list — but the boundary stays a joint (only
        // the internal leg leaves `joints`).
        space.unbind_back("00000001o", "00000002o", true);
        let chunk = space.chunk("00000001o").unwrap();
        assert_eq!(chunk.joints, vec![1]);
        assert_eq!(chunk.boundaries[0].bound_portals.len(), 1);
        let boundary = &chunk.boundaries[1];
        assert!(boundary.bound_portals.is_empty());
        assert_eq!(
            boundary
                .unbound_portals
                .iter()
                .map(|p| p.target.clone())
                .collect::<Vec<_>>(),
            vec![PortalTarget::Nothing]
        );

        // No matching portal left: a silent no-op. A missing owner too.
        space.unbind_back("00000001o", "00000002o", false);
        space.unbind_back("nosuchchunk", "00000002o", true);
        let chunk = space.chunk("00000001o").unwrap();
        assert_eq!(chunk.boundaries[1].unbound_portals.len(), 1);

        // An inside owner cut from an outside requester keeps the shell
        // hint: the portal is rewritten to INVASIVE before it unbinds.
        let mut shell = Chunk::new("00000000i", &identity_mapping(), &Matrix::identity());
        shell.boundaries = vec![ChunkBoundary {
            plane: PlaneEq::new(Vector3::new(1.0, 0.0, 0.0), 0.0),
            bound_portals: vec![face_portal("00000001o")],
            unbound_portals: Vec::new(),
        }];
        shell.joints = vec![0];
        space.add_chunk(shell);
        space.unbind_back("00000000i", "00000001o", true);
        let shell = space.chunk("00000000i").unwrap();
        assert!(shell.boundaries[0].bound_portals.is_empty());
        assert_eq!(
            shell.boundaries[0].unbound_portals[0].target,
            PortalTarget::Invasive
        );
    }

    /// The unload pass skips chunks within the hysteresis path and chunks
    /// whose bounds reach the camera's nearby bias, and takes at most one
    /// removable chunk per call (`chunk_manager.cpp:2236-2366`).
    #[test]
    fn unload_farthest_skips_the_hysteresis_path_and_the_camera_bias() {
        let mut space = ChunkSpace::new(0, vec![identity_mapping()]);
        for name in ["00000000o", "00000001o", "00000002o"] {
            let mut chunk = Chunk::new(name, &identity_mapping(), &Matrix::identity());
            chunk.is_loaded = true;
            chunk.is_bound = true;
            space.add_chunk(chunk);
        }
        // An inside shell at (50,0,50) whose covering outside chunk is
        // loaded: it clears the hysteresis and the bias (its own box is far
        // away) but the covering check keeps it (`chunk_manager.cpp:2250`).
        let mut shell = Chunk::new("00000000i", &identity_mapping(), &Matrix::identity());
        shell.transform.set_translate(50.0, 0.0, 50.0);
        shell.bounding_box = BoundBox::new(
            Vector3::new(50.0, -1.0, 50.0),
            Vector3::new(51.0, 1.0, 51.0),
        );
        shell.is_loaded = true;
        shell.is_bound = true;
        space.add_chunk(shell);

        // Chunk 1's origin (100,0,0) clears the 50-unit hysteresis, but its
        // bounds are stretched back over the camera grid corner: the bias
        // keeps it loaded.
        let near = space.chunk_mut("00000001o").unwrap();
        near.bounding_box = BoundBox::new(
            Vector3::new(-30.0, -1.0, -30.0),
            Vector3::new(200.0, 1.0, 200.0),
        );

        // Camera (50,0,50) → grid corner (0,0,0). The inside shell is
        // covered by its loaded outside chunk, chunk 0 sits at distance 0
        // (hysteresis), chunk 1 hits the bias, chunk 2 is taken.
        let unloaded = space.unload_farthest(&Vector3::new(50.0, 0.0, 50.0), 50.0);
        assert_eq!(unloaded, vec!["00000002o"]);
        assert!(space.chunk("00000000i").unwrap().is_loaded);
        assert!(space.chunk("00000000o").unwrap().is_loaded);
        assert!(space.chunk("00000001o").unwrap().is_loaded);
        let gone = space.chunk("00000002o").unwrap();
        assert!(!gone.is_loaded);
        assert!(!gone.is_bound);
        assert!(gone.items.is_empty());
        assert!(gone.bounds.is_empty());
        assert!(gone.joints.is_empty());

        // The chunk stays appointed by its space.
        assert!(space.chunk("00000002o").is_some());

        // Not loaded, not in the map: unload refuses without touching.
        assert!(!space.unload_chunk("00000002o"));
        assert!(!space.unload_chunk("nosuchchunk"));
        assert!(!space.unbind("nosuchchunk", false));
    }

    /// A locked chunk (`removable == false`) is skipped by the unload pass,
    /// and the pass takes at most `MAX_UNLOAD_CHUNKS` per call.
    #[test]
    fn unload_farthest_takes_one_removable_chunk_per_call() {
        let mut space = ChunkSpace::new(0, vec![identity_mapping()]);
        let mut locked = Chunk::new("00000000o", &identity_mapping(), &Matrix::identity());
        locked.is_loaded = true;
        locked.is_bound = true;
        locked.removable = false;
        let mut free = Chunk::new("00000001o", &identity_mapping(), &Matrix::identity());
        free.is_loaded = true;
        free.is_bound = true;
        space.add_chunk(locked);
        space.add_chunk(free);

        // The locked chunk is skipped, the free one is taken.
        let unloaded = space.unload_farthest(&Vector3::new(50.0, 0.0, 50.0), 50.0);
        assert_eq!(unloaded, vec!["00000001o"]);
        assert!(space.chunk("00000000o").unwrap().is_loaded);

        // A second pass finds nothing: the taken chunk is unbound, the
        // locked one is not removable.
        assert!(
            space
                .unload_farthest(&Vector3::new(50.0, 0.0, 50.0), 50.0)
                .is_empty()
        );
    }

    /// An inside shell whose covering outside chunk exists but never
    /// loaded holds nothing down: the shell itself is taken
    /// (`chunk_manager.cpp:2250-2256`).
    #[test]
    fn an_uncovered_inside_shell_unloads_with_its_outside_chunk() {
        let mut space = ChunkSpace::new(0, vec![identity_mapping()]);
        let mut shell = Chunk::new("00000000i", &identity_mapping(), &Matrix::identity());
        shell.transform.set_translate(50.0, 0.0, 50.0);
        shell.bounding_box = BoundBox::new(
            Vector3::new(50.0, -1.0, 50.0),
            Vector3::new(51.0, 1.0, 51.0),
        );
        shell.is_loaded = true;
        shell.is_bound = true;
        space.add_chunk(shell);
        // The covering chunk is created but not loaded.
        space.add_chunk(Chunk::new(
            "00000000o",
            &identity_mapping(),
            &Matrix::identity(),
        ));

        let unloaded = space.unload_farthest(&Vector3::new(50.0, 0.0, 50.0), 50.0);
        assert_eq!(unloaded, vec!["00000000i"]);
        assert!(!space.chunk("00000000i").unwrap().is_loaded);
        assert!(!space.chunk("00000000o").unwrap().is_loaded);
    }

    /// `AABB::intersects(v, bias)` (`boundbox.ipp:313-317`): inclusive at
    /// `min - bias`, half-open at `max + bias`.
    #[test]
    fn intersects_with_bias_is_half_open_at_the_inflated_max() {
        let bb = BoundBox::new(Vector3::new(0.0, 0.0, 0.0), Vector3::new(10.0, 10.0, 10.0));
        assert!(intersects_with_bias(
            &bb,
            &Vector3::new(-20.0, 5.0, 5.0),
            CAMERA_NEARBY_WIRED_BIAS
        ));
        assert!(!intersects_with_bias(
            &bb,
            &Vector3::new(-20.1, 5.0, 5.0),
            CAMERA_NEARBY_WIRED_BIAS
        ));
        // Exactly max + bias is outside; a hair less is inside.
        assert!(!intersects_with_bias(
            &bb,
            &Vector3::new(30.0, 5.0, 5.0),
            CAMERA_NEARBY_WIRED_BIAS
        ));
        assert!(intersects_with_bias(
            &bb,
            &Vector3::new(29.99, 5.0, 5.0),
            CAMERA_NEARBY_WIRED_BIAS
        ));
    }

    /// An inside chunk at world (x, 0, z) with the shell visual's six ±10
    /// planes and the `formBoundaries` bounds list: every face but the +z
    /// one, whose portal is internal (chunk.cpp:867-905).
    fn shell_inside(identifier: &str, mapping: &GeometryMapping, x: f32, z: f32) -> Chunk {
        let mut chunk = Chunk::new(identifier, mapping, &Matrix::identity());
        chunk.transform.set_translate(x, 0.0, z);
        // Inward normals, as `formBoundaries` installs them: the chunk hull
        // sits in the front half-space of every plane.
        let planar = [
            (Vector3::new(1.0, 0.0, 0.0), -10.0),
            (Vector3::new(-1.0, 0.0, 0.0), -10.0),
            (Vector3::new(0.0, 0.0, 1.0), -10.0),
            (Vector3::new(0.0, 0.0, -1.0), -10.0),
            (Vector3::new(0.0, 1.0, 0.0), -10.0),
            (Vector3::new(0.0, -1.0, 0.0), -10.0),
        ];
        chunk.boundaries = planar
            .iter()
            .map(|(normal, d)| ChunkBoundary {
                plane: PlaneEq::new(*normal, *d),
                bound_portals: Vec::new(),
                unbound_portals: Vec::new(),
            })
            .collect();
        // +x (external portal), -x wall, -z wall, earth, heaven — the +z
        // internal-portal face is a joint only.
        chunk.bounds = vec![0, 1, 3, 4, 5];
        chunk.bounding_box = BoundBox::new(
            Vector3::new(x - 10.0, -10.0, z - 10.0),
            Vector3::new(x + 10.0, 10.0, z + 10.0),
        );
        chunk
    }

    /// A mapping whose grid window covers a 3x3 block of cells, so a chunk in
    /// cell (1,2) has an in-window column.
    fn wide_mapping() -> GeometryMapping {
        GeometryMapping::new(
            "space/",
            100.0,
            GridBounds {
                min_l_grid_x: 0,
                min_l_grid_y: 0,
                max_l_grid_x: 2,
                max_l_grid_y: 2,
                min_grid_x: 0,
                min_grid_y: 0,
                max_grid_x: 2,
                max_grid_y: 2,
            },
            false,
        )
    }

    fn id(s: &str) -> String {
        s.to_string()
    }

    /// The BW-10-01-05 queries, as a free helper so a test can borrow the
    /// space mutably between a query and a focus pump.
    fn query_at(space: &mut ChunkSpace, x: f32, z: f32) -> Option<String> {
        space
            .find_chunk_from_point(&Vector3::new(x, 0.0, z))
            .map(|c| c.identifier.clone())
    }

    /// BW-10-01-05: the focus grid answers point queries per cell. `bind`
    /// queues each chunk for the next focus pump (`noticeChunk`,
    /// chunk.cpp:1082); `focus` creates the column of each focused chunk's
    /// centre. A point inside a loaded cell resolves to that cell's outside
    /// chunk; an in-window cell that never got a column and a point outside
    /// the grid window resolve to nothing.
    #[test]
    fn focus_grid_answers_point_queries_per_cell_and_blank_cells_are_empty() {
        let mapping = wide_mapping();
        let mut space = ChunkSpace::new(0, vec![mapping.clone()]);
        for name in ["00000000o", "00010000o", "00000001o", "00010001o"] {
            let mut chunk = Chunk::new(name, &mapping, &Matrix::identity());
            chunk.is_loaded = true;
            space.add_chunk(chunk);
            assert!(space.bind(name));
        }
        // Bound but not yet pumped: no column exists, so nothing resolves.
        assert_eq!(
            space
                .find_chunk_from_point(&Vector3::new(50.0, 0.0, 50.0))
                .map(|c| c.identifier.clone()),
            None
        );
        space.focus();

        assert_eq!(query_at(&mut space, 50.0, 50.0), Some(id("00000000o")));
        assert_eq!(query_at(&mut space, 150.0, 50.0), Some(id("00010000o")));
        assert_eq!(query_at(&mut space, 50.0, 150.0), Some(id("00000001o")));
        assert_eq!(query_at(&mut space, 150.0, 150.0), Some(id("00010001o")));
        // Cell (2,0) is in the window but never got a column.
        assert_eq!(query_at(&mut space, 250.0, 50.0), None);
        // Outside the grid window entirely.
        assert_eq!(query_at(&mut space, -50.0, 50.0), None);
        assert_eq!(query_at(&mut space, 50.0, -50.0), None);
        assert_eq!(query_at(&mut space, 350.0, 350.0), None);
    }

    /// BW-10-01-05: `findChunkFromPointExact` gates on `completed()`
    /// (chunk_space.cpp:757-770). A bound-but-unfocused chunk is not
    /// completed, so the exact query is empty until the focus pump runs.
    #[test]
    fn the_exact_query_gates_on_the_focus_pump() {
        let mapping = wide_mapping();
        let mut space = ChunkSpace::new(0, vec![mapping.clone()]);
        let mut chunk = Chunk::new("00000000o", &mapping, &Matrix::identity());
        chunk.is_loaded = true;
        space.add_chunk(chunk);
        assert!(space.bind("00000000o"));

        let point = Vector3::new(50.0, 0.0, 50.0);
        assert!(space.find_chunk_from_point_exact(&point).is_none());
        space.focus();
        assert_eq!(
            space
                .find_chunk_from_point_exact(&point)
                .map(|c| c.identifier.clone()),
            Some(id("00000000o"))
        );
    }

    /// BW-10-01-05: `ignoreChunk` (chunk_space.cpp:1300-1355) erases the 3x3
    /// block of columns around a bound chunk's centre, so every query in
    /// those cells comes back empty until the next focus pump rebuilds them
    /// from the blurred holders. An unbound chunk refuses to be ignored.
    #[test]
    fn ignore_chunk_erases_the_surrounding_columns_until_the_next_focus() {
        let mapping = wide_mapping();
        let mut space = ChunkSpace::new(0, vec![mapping.clone()]);
        for name in ["00000000o", "00010000o", "00000001o", "00010001o"] {
            let mut chunk = Chunk::new(name, &mapping, &Matrix::identity());
            chunk.is_loaded = true;
            space.add_chunk(chunk);
            assert!(space.bind(name));
        }
        space.focus();

        assert_eq!(query_at(&mut space, 50.0, 50.0), Some(id("00000000o")));
        assert_eq!(query_at(&mut space, 150.0, 150.0), Some(id("00010001o")));

        // Ignoring the cell (1,1) chunk erases the whole 3x3 block x,z in
        // 0..=2, so both queries come back empty.
        space.ignore_chunk("00010001o");
        assert_eq!(query_at(&mut space, 50.0, 50.0), None);
        assert_eq!(query_at(&mut space, 150.0, 50.0), None);
        assert_eq!(query_at(&mut space, 150.0, 150.0), None);

        // The next focus pump rebuilds the columns of the *other* holders,
        // which the deleted columns blurred. The ignored chunk itself is not
        // re-blurred: `removeFromBlurred` pulls it off the list after the
        // column deletion pushed it back on, so its column stays gone
        // (`chunk_space.cpp:1352`).
        space.focus();
        assert_eq!(query_at(&mut space, 50.0, 50.0), Some(id("00000000o")));
        assert_eq!(query_at(&mut space, 150.0, 50.0), Some(id("00010000o")));
        assert_eq!(query_at(&mut space, 50.0, 150.0), Some(id("00000001o")));
        assert_eq!(query_at(&mut space, 150.0, 150.0), None);

        // A chunk that is no longer bound refuses to be ignored: the
        // reference's `!isBound()` guard leaves its column intact.
        let mut far = Chunk::new("00020001o", &mapping, &Matrix::identity());
        far.is_loaded = true;
        space.add_chunk(far);
        assert!(space.bind("00020001o"));
        space.focus();
        assert_eq!(query_at(&mut space, 250.0, 150.0), Some(id("00020001o")));
        space.unbind("00020001o", false);
        space.ignore_chunk("00020001o");
        assert_eq!(query_at(&mut space, 250.0, 150.0), Some(id("00020001o")));
    }

    /// BW-10-01-05: `recalcGridBounds` (chunk_space.cpp:1077-1123 +
    /// server_chunk_space.cpp:132-176) resizes the column window to the
    /// mappings' rect. A column that leaves the window is deleted, which
    /// smudges its bound holder into the blurred list; columns that stay in
    /// the window keep their chunks.
    #[test]
    fn recalc_grid_bounds_drops_the_columns_that_leave_the_window() {
        let mapping = wide_mapping();
        let mut space = ChunkSpace::new(0, vec![mapping.clone()]);

        // An inside chunk whose shell sits in cell (1,2).
        let mut inside = shell_inside("00000000i", &mapping, 150.0, 250.0);
        inside.is_loaded = true;
        space.add_chunk(inside);
        assert!(space.bind("00000000i"));
        for name in ["00000000o", "00010000o", "00000001o", "00010001o"] {
            let mut chunk = Chunk::new(name, &mapping, &Matrix::identity());
            chunk.is_loaded = true;
            space.add_chunk(chunk);
            assert!(space.bind(name));
        }
        space.focus();

        assert_eq!(query_at(&mut space, 150.0, 250.0), Some(id("00000000i")));
        assert_eq!(query_at(&mut space, 50.0, 50.0), Some(id("00000000o")));

        // Narrow the window to cells 0..=1: column (1,2) leaves it and is
        // deleted (smudging the inside chunk), the four origin columns stay.
        space.mappings[0].bounds.max_l_grid_x = 1;
        space.mappings[0].bounds.max_grid_x = 1;
        space.mappings[0].bounds.max_l_grid_y = 1;
        space.mappings[0].bounds.max_grid_y = 1;
        space.recalc_grid_bounds();
        space.focus();

        // The inside chunk's cell is now out of the window: nothing resolves
        // there even though the chunk is still bound.
        assert_eq!(query_at(&mut space, 150.0, 250.0), None);
        // The in-window columns survived the resize untouched.
        assert_eq!(query_at(&mut space, 50.0, 50.0), Some(id("00000000o")));
        assert_eq!(query_at(&mut space, 150.0, 150.0), Some(id("00010001o")));
    }

    /// BW-10-01-05: `findChunkFromPoint`'s outside arm
    /// (`chunk_space.cpp:714-727`) answers straight from the column's outside
    /// chunk, without the hull walk.
    #[test]
    fn the_outside_query_answers_without_the_hull_walk() {
        let mapping = wide_mapping();
        let mut space = ChunkSpace::new(0, vec![mapping.clone()]);
        for name in ["00000000o", "00010000o"] {
            let mut chunk = Chunk::new(name, &mapping, &Matrix::identity());
            chunk.is_loaded = true;
            space.add_chunk(chunk);
            assert!(space.bind(name));
        }
        space.focus();

        assert_eq!(
            space
                .find_outside_chunk_from_point(&Vector3::new(50.0, 0.0, 50.0))
                .map(|c| c.identifier.clone()),
            Some(id("00000000o"))
        );
        // Cell (1,1) never got a column.
        assert!(
            space
                .find_outside_chunk_from_point(&Vector3::new(150.0, 0.0, 150.0))
                .is_none()
        );
    }

    /// BW-10-01-05: `findChunkFromPointExact` (`chunk_space.cpp:753-770`)
    /// also gates on `Chunk::completed` — a chunk that is no longer focussed is
    /// not a valid answer even though it sits in the column.
    #[test]
    fn the_exact_query_refuses_an_incomplete_chunk() {
        let mapping = wide_mapping();
        let mut space = ChunkSpace::new(0, vec![mapping.clone()]);
        let mut chunk = Chunk::new("00000000o", &mapping, &Matrix::identity());
        chunk.is_loaded = true;
        space.add_chunk(chunk);
        assert!(space.bind("00000000o"));
        space.focus();

        let point = Vector3::new(50.0, 0.0, 50.0);
        assert_eq!(
            space
                .find_chunk_from_point_exact(&point)
                .map(|c| c.identifier.clone()),
            Some(id("00000000o"))
        );
        // Drop the focus count: the plain query still answers, the exact one
        // does not.
        space.chunks.get_mut("00000000o").unwrap().focus_count = 0;
        assert_eq!(query_at(&mut space, 50.0, 50.0), Some(id("00000000o")));
        assert!(space.find_chunk_from_point_exact(&point).is_none());
    }

    /// BW-10-01-05: `blurredChunk` (`base_chunk_space.cpp:720-730`) refuses to
    /// park an unbound chunk, so the pump has nothing to focus.
    #[test]
    fn blurring_an_unbound_chunk_parks_nothing() {
        let mapping = wide_mapping();
        let mut space = ChunkSpace::new(0, vec![mapping.clone()]);
        let mut chunk = Chunk::new("00000000o", &mapping, &Matrix::identity());
        chunk.is_loaded = true;
        space.add_chunk(chunk);

        // Loaded but never bound: the blur is refused and no column is built.
        space.notice_chunk("00000000o");
        space.focus();
        assert_eq!(query_at(&mut space, 50.0, 50.0), None);

        // Binding is what queues the chunk for the pump.
        assert!(space.bind("00000000o"));
        space.focus();
        assert_eq!(query_at(&mut space, 50.0, 50.0), Some(id("00000000o")));
    }

    /// BW-10-01-05: with no mappings there is no grid window at all
    /// (`server_chunk_space.cpp:132-137`), so the column grid collapses. The
    /// constructor's `recalcGridBounds` leg (`chunk_space.cpp:230`) is where
    /// the reference reaches that arm.
    #[test]
    fn recalc_grid_bounds_with_no_mappings_collapses_the_window() {
        let mut space = ChunkSpace::new(0, vec![]);
        space.recalc_grid_bounds();
        // No cell is in span: the window is 0x0.
        assert!(!space.columns.in_span(0, 0));
        assert!(!space.columns.in_span(-1, -1));
        assert_eq!(space.columns.span(), (0, 0, 0, 0));
    }
}
