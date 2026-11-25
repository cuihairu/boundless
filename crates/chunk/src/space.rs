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
//! are BW-10-02+); unload is BW-10-01-04.

use std::collections::BTreeMap;

use foundation::math::{Matrix, Vector3};
use physics::{
    CollisionCallback, CollisionState, QuadTree, SweepParams, SweepShape, collide_into_object_point,
};
use resources::datasection::DataSection;

use crate::boundary::PortalTarget;
use crate::chunk::Chunk;
use crate::column::{Column, ColumnGrid};
use crate::grid_traversal::SpaceGridTraversal;
use crate::identifier::is_outside_chunk;
use crate::mapping::GeometryMapping;
use crate::obstacle::{ChunkObstacle, next_mark};

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
    /// `currentFocus_` (`server_chunk_space.hpp:114`).
    focus: ColumnGrid,
    /// `blurred_` (`base_chunk_space.hpp`): bound chunks whose hulls are
    /// not in the focus grid yet.
    blurred: Vec<String>,
    /// The obstacle arena. The reference scatters refcounted
    /// `ChunkObstacle`s behind the columns' `heldObstacles_` lists and
    /// the model obstacles' own vectors; the port owns them here and the
    /// column trees store arena indices (registered).
    obstacles: Vec<ChunkObstacle>,
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
            obstacles: Vec::new(),
        };
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

    /// `ChunkModelObstacle::addObstacle` + `addToSpace`
    /// (`chunk_model_obstacle.cpp:186-270`): the obstacle joins the
    /// arena (the model obstacle's `obstacles_` list), and a focussed
    /// chunk's obstacle is registered into every column its world
    /// bounding box touches — the eight corners decide which, and a
    /// column shut to the chunk refuses it. `numColumns == 0` logs an
    /// ERROR_MSG in the reference ("Didn't add obstacle to any
    /// columns!"); the port has no logging face (registered).
    pub fn add_obstacle(&mut self, obstacle: ChunkObstacle) -> usize {
        let index = self.obstacles.len() as u32;

        // the focussed gate: only a focussed chunk's obstacles reach the
        // columns (the reference checks pChunk()->focussed() first)
        let focussed = obstacle
            .chunk()
            .is_some_and(|id| self.chunk(id).is_some_and(|c| c.focus_count > 0));

        if focussed {
            let bb = &obstacle.bb;
            for i in 0..8 {
                // set up pt to be the corner in the bounding box
                let pt = Vector3::new(
                    if i & 1 != 0 { bb.max.x } else { bb.min.x },
                    if i & 2 != 0 { bb.max.y } else { bb.min.y },
                    if i & 4 != 0 { bb.max.z } else { bb.min.z },
                );
                if let Some(column) = self.column(&pt, true) {
                    column.add_obstacle(&obstacle, index);
                }
            }
            // a corner touching no column means the obstacle sits wholly
            // outside the focus grid — the reference ERROR_MSGs on that
            // ("Didn't add obstacle to any columns!"); no port logging
            // face (registered)
        }

        self.obstacles.push(obstacle);
        index as usize
    }

    /// `ChunkSpace::collide` (`chunk_space.cpp:144-158`), the ray
    /// instantiation of `ChunkSpace_collide` (`chunk_space.cpp:95-131`):
    /// sweep a ray through the focus grid's columns, each in the
    /// travel window the grid walk reports, until a callback stops the
    /// sweep or the walk runs out (the swept-triangle overload has no
    /// port-side space face — registered). The ±100000 `MF_ASSERT_DEV`
    /// on the endpoints is a dev-only stray-coordinate guard (the port
    /// keeps the reference's silent release behaviour).
    pub fn collide(&self, start: Vector3, end: Vector3, cc: &mut dyn CollisionCallback) -> f32 {
        // increment the hull obstacle mark
        next_mark();

        let sp = SweepParams::new(&SweepShape::Ray(start), end);

        // and make the grid traversal object
        let mut sgt = SpaceGridTraversal::new(
            sp.bsource(),
            sp.bextent(),
            sp.shape_range(),
            self.grid_size(),
        );

        // make the collision state object
        let mut cs = CollisionState::new(cc);

        loop {
            let in_span = self.focus.in_span(sgt.sx, sgt.sz);
            let p_col = self.focus.get(sgt.sx, sgt.sz);

            // check this column as long as it's in range
            if in_span
                && let Some(column) = p_col
                && self.traverse_obstacle_tree(&sp, &mut cs, &sgt, column.obstacles())
            {
                return cs.dist;
            }

            if !(!cs.only_less || (cs.dist + sp.shape_radius() > sgt.cell_e_travel)) {
                break;
            }
            if !sgt.next() {
                break;
            }
        }

        cs.dist
    }

    /// `traverseObstacleTree` (`chunk_space.cpp:47-82`), the ray
    /// instantiation: the hull-tree walk runs from the window's cast
    /// point to its land point (widened by the shape radius on both
    /// ends), each obstacle is deduped through the mark and swept
    /// through `SweepHelpers::collideIntoObject`.
    fn traverse_obstacle_tree(
        &self,
        sp: &SweepParams,
        cs: &mut CollisionState,
        sgt: &SpaceGridTraversal,
        tree: &QuadTree,
    ) -> bool {
        // traverse the hulltree from cast to land
        let mut htt = tree.traverse(
            &(sp.csource() + sp.direction() * (sgt.cell_s_travel - sp.shape_radius())),
            &(sp.csource() + sp.direction() * (sgt.cell_e_travel + sp.shape_radius())),
            sp.shape_radius(),
        );

        while let Some(index) = htt.next() {
            let obstacle = &self.obstacles[index as usize];

            if obstacle.mark() || obstacle.chunk().is_none() {
                continue;
            }

            //dprintf( "Considering obstacle 0x%08X\n", &pObstacle->data_ );

            if collide_into_object_point(
                sp,
                obstacle,
                &obstacle.bb,
                &obstacle.transform_inverse,
                cs,
            ) {
                return true;
            }
        }

        false
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

    /// `Chunk::unbind(bool)` (`chunk.cpp:1305-1458`): move every connected
    /// bound portal back to its boundary's unbound list and tell the online
    /// neighbour to do the same, then drop `isBound_`. With `cut` the
    /// portal targets are cleared on both sides — an inside chunk's portal
    /// to an outside chunk becomes `INVASIVE`, everything else `NULL` — so
    /// the connection cannot silently rebind; without it the target names
    /// survive and a later `bind` reconnects without re-reading anything.
    ///
    /// The reference's exit-portal item removal and lender/borrower
    /// release have no port-side objects and stay registered (see
    /// `docs/review/bw10-01-chunk-load.md`); the focus-grid eviction is
    /// ported (`ignoreChunk`, BW-10-01-05).
    pub fn unbind(&mut self, identifier: &str, cut: bool) {
        // `pSpace_->ignoreChunk( this ); focusCount_ = 0; updateCompleted()`
        // (`chunk.cpp:1332-1337`): nothing of this chunk may stay findable
        // in the focus grid
        self.ignore_chunk(identifier);
        // Decide immutably which bound portals move back, what their
        // targets become, and what each neighbour must do.
        struct Move {
            joint: usize,
            portal: usize,
            target: PortalTarget,
        }
        let mut moves: Vec<Move> = Vec::new();
        struct Back {
            other: String,
            cut: bool,
        }
        let mut backs: Vec<Back> = Vec::new();
        {
            let Some(chunk) = self.chunks.get(identifier) else {
                return;
            };
            for &ji in &chunk.joints {
                for (pi, portal) in chunk.boundaries[ji].bound_portals.iter().enumerate() {
                    match &portal.target {
                        // A heavenly portal in an inside chunk unbinds
                        // itself (in an outside chunk it has no chunk and
                        // stays bound, like every other special target).
                        PortalTarget::Heaven if !chunk.is_outside => {
                            moves.push(Move {
                                joint: ji,
                                portal: pi,
                                target: PortalTarget::Heaven,
                            });
                        }
                        // don't unbind it if it's not a chunk
                        target if !target.has_chunk() => {}
                        PortalTarget::Chunk(other) => {
                            // clear the chunk if we're cutting it off
                            let target = if cut {
                                if !chunk.is_outside && is_outside_chunk(other) {
                                    PortalTarget::Invasive
                                } else {
                                    PortalTarget::Nothing
                                }
                            } else {
                                portal.target.clone()
                            };
                            // always cut off an exit portal into an
                            // outside chunk
                            let back_cut = if chunk.is_outside && !is_outside_chunk(other) {
                                true
                            } else {
                                cut
                            };
                            moves.push(Move {
                                joint: ji,
                                portal: pi,
                                target,
                            });
                            backs.push(Back {
                                other: other.clone(),
                                cut: back_cut,
                            });
                        }
                        _ => {}
                    }
                }
            }
        }

        // Move them back to the unbound lists (`ChunkBoundary::unbindPortal`,
        // ascending bound index, which is the reference's `unbindPortal
        // (i--)` append order).
        if let Some(chunk) = self.chunks.get_mut(identifier) {
            let mut touched: Vec<usize> = Vec::new();
            for mv in &moves {
                if !touched.contains(&mv.joint) {
                    touched.push(mv.joint);
                }
            }
            for ji in touched {
                let joint_moves: Vec<(usize, PortalTarget)> = moves
                    .iter()
                    .filter(|m| m.joint == ji)
                    .map(|m| (m.portal, m.target.clone()))
                    .collect();
                let boundary = &mut chunk.boundaries[ji];
                let old = std::mem::take(&mut boundary.bound_portals);
                let mut moved = Vec::new();
                let mut kept = Vec::new();
                for (i, mut portal) in old.into_iter().enumerate() {
                    if let Some((_, target)) = joint_moves.iter().find(|(pi, _)| *pi == i) {
                        portal.target = target.clone();
                        moved.push(portal);
                    } else {
                        kept.push(portal);
                    }
                }
                boundary.unbound_portals.extend(moved);
                boundary.bound_portals = kept;
            }
            chunk.is_bound = false;
            chunk.focus_count = 0;
        }

        // and let the neighbours know we're offline
        for back in backs {
            self.unbind_back(&back.other, identifier, back.cut);
        }
    }

    /// `Chunk::unbind(Chunk*, bool)` (`chunk.cpp:2083-2138`): in `owner`,
    /// find the first bound portal pointing back at `requester`; with
    /// `cut` clear its target — erasing the whole joint when the portal is
    /// internal — otherwise move it to the unbound list. A missing owner
    /// or a missing reverse portal is a logged no-op in the reference;
    /// the port stays silent and registers the divergence.
    fn unbind_back(&mut self, owner: &str, requester: &str, cut: bool) {
        let Some(chunk) = self.chunks.get_mut(owner) else {
            return;
        };
        for (joint_pos, &ji) in chunk.joints.iter().enumerate() {
            let boundary = &mut chunk.boundaries[ji];
            let Some(pi) = boundary
                .bound_portals
                .iter()
                .position(|p| p.target == PortalTarget::Chunk(requester.to_string()))
            else {
                continue;
            };
            if cut {
                // clear the link if we're cutting it out
                boundary.bound_portals[pi].target =
                    if !chunk.is_outside && is_outside_chunk(requester) {
                        PortalTarget::Invasive
                    } else {
                        PortalTarget::Nothing
                    };
                // get rid of the whole boundary if this was an internal
                // portal on a non-bounding plane
                if boundary.bound_portals[pi].internal {
                    chunk.joints.remove(joint_pos);
                }
            } else {
                let portal = boundary.bound_portals.remove(pi);
                boundary.unbound_portals.push(portal);
            }
            return;
        }
    }

    /// `ChunkSpace::unloadChunk` (`chunk_space.cpp:1262-1296`): unbind then
    /// unload one chunk. Unloaded or (here, never in-flight — the sync
    /// loader has no loading state, registered) loading chunks answer
    /// false; a non-removable chunk answers false. The chunk stays in the
    /// map, appointed but unloaded.
    pub fn unload_chunk(&mut self, identifier: &str) -> bool {
        let Some(chunk) = self.chunks.get(identifier) else {
            return false;
        };

        // Can only unload if chunk is removable
        if !chunk.removable {
            return false;
        }

        // Cannot unload if chunk is not loaded
        if !chunk.is_loaded {
            return false;
        }

        // Unbind, then unload
        if chunk.is_bound {
            self.unbind(identifier, false);
        }

        // Unload after unbind
        let chunk = self.chunks.get_mut(identifier).unwrap();
        chunk.unload();

        true
    }

    /// `ChunkSpace::unloadChunks`
    /// (`chunk_space.cpp:1204-1222, 1235-1246`): attempt to unbind and unload
    /// every loaded chunk, map order, returning the identifiers that went.
    pub fn unload_chunks(&mut self) -> Vec<String> {
        let identifiers: Vec<String> = self.chunks.keys().cloned().collect();
        identifiers
            .into_iter()
            .filter(|id| self.unload_chunk(id))
            .collect()
    }

    // ------------------------------------------------------------------
    // BW-10-01-05: the focus grid and the point queries over it
    // ------------------------------------------------------------------

    /// `ChunkSpace::recalcGridBounds` (`chunk_space.cpp:1077-1127`): the
    /// focus window spans the mappings' local grid bounds; columns dropped
    /// by the resize smudge their chunks (`~Column` deletes the
    /// `HullChunk` holdings, each of which smudges its chunk,
    /// `base_chunk_space.cpp:41-43`).
    pub fn recalc_grid_bounds(&mut self) {
        let (min_x, min_z, max_x, max_z) = if self.mappings.is_empty() {
            (0, 0, 0, 0)
        } else {
            let mut min_x = i32::MAX;
            let mut min_z = i32::MAX;
            let mut max_x = i32::MIN;
            let mut max_z = i32::MIN;
            for m in &self.mappings {
                min_x = min_x.min(m.bounds.min_l_grid_x);
                min_z = min_z.min(m.bounds.min_l_grid_y);
                max_x = max_x.max(m.bounds.max_l_grid_x);
                max_z = max_z.max(m.bounds.max_l_grid_y);
            }
            (min_x, min_z, max_x, max_z)
        };
        let mut dropped = Vec::new();
        self.focus.rect(
            min_x,
            min_z,
            max_x - min_x + 1,
            max_z - min_z + 1,
            &mut dropped,
        );
        self.release_dropped_columns(dropped);
    }

    /// The `~Column` face (`base_chunk_space.cpp:393-411`): every held
    /// `HullChunk` smudges its chunk — the focus count drops and a bound
    /// chunk joins the blurred list.
    fn release_dropped_columns(&mut self, dropped: Vec<Column>) {
        for column in dropped {
            for id in column.holdings() {
                self.smudge_chunk(id);
            }
        }
    }

    /// `Chunk::smudge` (`chunk.cpp:1619-1638`): a non-zero focus count
    /// drops to zero and the (bound) chunk joins the blurred list.
    pub fn smudge_chunk(&mut self, identifier: &str) {
        if self.chunk(identifier).is_some_and(|c| c.focus_count != 0) {
            if let Some(c) = self.chunks.get_mut(identifier) {
                c.focus_count = 0;
            }
            self.blurred_chunk(identifier);
        }
    }

    /// `BaseChunkSpace::blurredChunk` (`base_chunk_space.cpp:718-726`):
    /// but only if it's bound.
    pub fn blurred_chunk(&mut self, identifier: &str) {
        if self.chunk(identifier).is_some_and(|c| c.is_bound) {
            self.blurred.push(identifier.to_string());
        }
    }

    /// `BaseChunkSpace::removeFromBlurred` (`base_chunk_space.cpp:729-744`).
    pub fn remove_from_blurred(&mut self, identifier: &str) -> bool {
        if let Some(pos) = self.blurred.iter().position(|s| s == identifier) {
            self.blurred.remove(pos);
            return true;
        }
        false
    }

    /// The chunks waiting to be (re)focussed.
    pub fn blurred_chunks(&self) -> &[String] {
        &self.blurred
    }

    /// The focus grid window (`xBegin/zBegin/xEnd/zEnd`).
    pub fn focus_span(&self) -> (i32, i32, i32, i32) {
        self.focus.span()
    }

    /// `ChunkSpace::noticeChunk` (`chunk_space.cpp:1356-1371`): the chunk
    /// is now bound and may be focussed.
    pub fn notice_chunk(&mut self, identifier: &str) {
        self.blurred_chunk(identifier);
    }

    /// The grid cell of a chunk centre — the reference computes it with an
    /// int truncation plus a manual negative adjust
    /// (`server_chunk_space.cpp:55-58`, `chunk_space.cpp:1320-1321`).
    fn centre_grid(&self, centre: f32) -> i32 {
        let mut n = (centre / self.grid_size()) as i32;
        if centre < 0.0 {
            n -= 1;
        }
        n
    }

    /// `ChunkSpace::column(point, canCreate)` (`chunk_space.cpp:776-803`):
    /// the column at the point, or None when out of the focus span (or not
    /// created and `can_create` is false).
    pub fn column(&mut self, point: &Vector3, can_create: bool) -> Option<&mut Column> {
        let x = self.point_to_grid(point.x);
        let z = self.point_to_grid(point.z);
        // check range
        if !self.focus.in_span(x, z) {
            return None;
        }
        // create if willing and able
        if can_create && self.focus.get(x, z).is_none() {
            self.focus
                .set(x, z, Some(Column::new(x, z, self.grid_size())));
        }
        self.focus.get_mut(x, z)
    }

    /// `Chunk::focus` (`chunk.cpp:1536-1618`): build the chunk's world
    /// border, add it to the column(s) it overlaps — the chunk centre for
    /// an outside chunk, the eight bounding-box corners for an inside one —
    /// and set the focus count to one.
    pub fn focus_chunk(&mut self, identifier: &str) {
        let Some(chunk) = self.chunks.get(identifier) else {
            return;
        };
        let border = chunk.focus_border();
        let is_outside = chunk.is_outside;
        let centre = chunk.centre();
        let bounding_box = chunk.bounding_box;

        // find what columns we need to add to (z is needless I know)
        let mut cells: Vec<(i32, i32)> = Vec::new();
        if is_outside {
            cells.push((self.point_to_grid(centre.x), self.point_to_grid(centre.z)));
        } else {
            for i in 0..8 {
                let pt = Vector3::new(
                    if i & 1 != 0 {
                        bounding_box.max.x
                    } else {
                        bounding_box.min.x
                    },
                    if i & 2 != 0 {
                        bounding_box.max.y
                    } else {
                        bounding_box.min.y
                    },
                    if i & 4 != 0 {
                        bounding_box.max.z
                    } else {
                        bounding_box.min.z
                    },
                );
                let cell = (self.point_to_grid(pt.x), self.point_to_grid(pt.z));
                if !cells.contains(&cell) {
                    cells.push(cell);
                }
            }
        }

        // and add it to all of them (the reference asserts each one)
        for (x, z) in cells {
            if !self.focus.in_span(x, z) {
                continue;
            }
            if self.focus.get(x, z).is_none() {
                self.focus
                    .set(x, z, Some(Column::new(x, z, self.grid_size())));
            }
            if let Some(column) = self.focus.get_mut(x, z) {
                column.add_chunk(&border, self.chunks.get(identifier).unwrap());
            }
        }

        // focus any current caches — no port-side caches, registered

        // and set our focus count to one (`chunk.cpp:1604-1606`)
        if let Some(chunk) = self.chunks.get_mut(identifier) {
            chunk.focus_count = 1;
        }
    }

    /// `ServerChunkSpace::focus` (`server_chunk_space.cpp:50-113`): focus
    /// every blurred chunk — shut the columns that have already seen it,
    /// refocus it, then mark the neighbourhood as having seen it again.
    pub fn focus(&mut self) {
        while !self.blurred.is_empty() {
            let identifier = self.blurred.remove(0);

            // find out where it is in the focus grid; a chunk missing from
            // the map is a stale blur entry the port skips (the reference
            // walks a raw pointer)
            let Some(chunk) = self.chunks.get(&identifier) else {
                continue;
            };
            let centre = chunk.centre();
            let nx = self.centre_grid(centre.x);
            let nz = self.centre_grid(centre.z);

            // see if this chunk is new to (nx,nz) and adjacent columns,
            // close them if it isn't
            for x in nx - 1..=nx + 1 {
                for z in nz - 1..=nz + 1 {
                    if let Some(column) = self.focus.get_mut(x, z) {
                        column.shut_if_seen(&identifier);
                    }
                }
            }

            // do the actual focussing work
            self.focus_chunk(&identifier);

            // open all the columns, and mark them as having seen this chunk
            for x in nx - 1..=nx + 1 {
                for z in nz - 1..=nz + 1 {
                    if let Some(column) = self.focus.get_mut(x, z) {
                        column.open_and_see(&identifier);
                    }
                }
            }
        }
    }

    /// `ChunkSpace::ignoreChunk` (`chunk_space.cpp:1306-1353`): the chunk
    /// is about to be disposed — delete the columns of its 3×3 focus
    /// neighbourhood (deleting, not marking stale, so nothing of this
    /// chunk stays findable in the focus grid) and take it out of the
    /// blurred list.
    pub fn ignore_chunk(&mut self, identifier: &str) {
        // can't ignore unbound chunks
        if !self.chunk(identifier).is_some_and(|c| c.is_bound) {
            // ERROR_MSG "Attempted to ignore offline chunk"
            return;
        }

        let centre = self.chunk(identifier).unwrap().centre();
        let nx = self.centre_grid(centre.x);
        let nz = self.centre_grid(centre.z);

        for x in nx - 1..=nx + 1 {
            for z in nz - 1..=nz + 1 {
                if self.focus.in_span(x, z) {
                    let dropped = self.focus.set(x, z, None);
                    self.release_dropped_columns(dropped.into_iter().collect());
                }
            }
        }

        self.remove_from_blurred(identifier);
    }

    /// `ChunkSpace::findOutsideChunkFromPoint` (`chunk_space.cpp:714-734`):
    /// the last-resort outside-chunk lookup, with the small +y fudge the
    /// reference adds to dodge bounding-box edge floating point.
    pub fn find_outside_chunk_from_point(&self, point: &Vector3) -> Option<String> {
        let mut pt = *point;
        pt.y += 0.0001;
        let column = self.column_at(&pt)?;
        column.outside_chunk().map(|id| id.to_string())
    }

    /// `column(pt, false)` (`chunk_space.cpp:788-802`): the read-only
    /// column lookup.
    fn column_at(&self, point: &Vector3) -> Option<&Column> {
        let x = self.point_to_grid(point.x);
        let z = self.point_to_grid(point.z);
        if !self.focus.in_span(x, z) {
            return None;
        }
        self.focus.get(x, z)
    }

    /// `ChunkSpace::findChunkFromPoint` (`chunk_space.cpp:731-751`) plus
    /// `BaseChunkSpace::Column::findChunk` (`base_chunk_space.cpp:570-595`):
    /// the outside chunk of the point's grid cell, displaced by any
    /// smaller-volume inside chunk whose bounding box contains the point
    /// (equal volume keeps the first chunk in map order). Like the reference
    /// a tiny fudge is added to y so points sitting exactly on a chunk face
    /// still resolve.
    pub fn find_chunk_from_point(&self, point: &Vector3) -> Option<&Chunk> {
        let mut pt = *point;
        pt.y += 0.0001;
        let grid_x = self.point_to_grid(pt.x);
        let grid_z = self.point_to_grid(pt.z);
        let mut winner = self.find_chunk_by_grid(grid_x, grid_z);
        for chunk in self.chunks.values() {
            if chunk.is_outside || !chunk.bounding_box.contains(&pt) {
                continue;
            }
            let smaller = match winner {
                // any containing inside chunk beats the column's outside chunk
                None => true,
                Some(w) if w.is_outside => true,
                Some(w) => chunk_volume(w) > chunk_volume(chunk),
            };
            if smaller {
                winner = Some(chunk);
            }
        }
        winner
    }

    /// `ChunkSpace::findChunkFromPointExact` (`chunk_space.cpp:757-770`):
    /// [`find_chunk_from_point`] plus the `completed()` check — a chunk that
    /// has not finished loading is not a valid drop target.
    pub fn find_chunk_from_point_exact(&self, point: &Vector3) -> Option<&Chunk> {
        match self.find_chunk_from_point(point) {
            Some(chunk) if chunk.is_loaded => Some(chunk),
            _ => None,
        }
    }

    /// Drop a chunk from the map (`ChunkSpace::removeChunk`).
    pub fn remove_chunk(&mut self, identifier: &str) {
        self.chunks.remove(identifier);
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
}

/// `Chunk::volume` (`chunk.cpp:3506-3511`): the bounding box extents product.
fn chunk_volume(chunk: &Chunk) -> f32 {
    let v = chunk.bounding_box.max - chunk.bounding_box.min;
    v.x * v.y * v.z
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
    use crate::obstacle::test_bsp::slab_obstacle;
    use foundation::math::{BoundBox, PlaneEq};
    use physics::{COLLIDE_STOP, ClosestTriangle, CollisionObstacle, WorldTriangle};

    /// `COLLIDE_STOP` on every visit — the sweep stops at the first answer.
    struct StoppingCallback;

    impl CollisionCallback for StoppingCallback {
        fn do_visit(
            &mut self,
            _obstacle: &CollisionObstacle,
            _triangle: &WorldTriangle,
            _dist: f32,
        ) -> i32 {
            COLLIDE_STOP
        }
    }

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
    fn add_outside_chunk(space: &mut ChunkSpace, mapping: &GeometryMapping, id: &str) {
        let mut chunk = Chunk::new(id, mapping, &Matrix::identity());
        chunk.is_loaded = true;
        space.add_chunk(chunk);
    }

    /// An inside chunk with a box shell around (10,0,10)-(20,10,20): six
    /// inward boundary planes indexed by `bounds`, the input `Chunk::focus`
    /// folds into its columns' hull trees.
    fn shell_chunk(mapping: &GeometryMapping) -> Chunk {
        let mut chunk = Chunk::new("00000000i", mapping, &Matrix::identity());
        let min = Vector3::new(10.0, 0.0, 10.0);
        let max = Vector3::new(20.0, 10.0, 20.0);
        chunk.bounding_box = foundation::math::BoundBox::new(min, max);
        chunk.boundaries = [
            (Vector3::new(1.0, 0.0, 0.0), min.x),
            (Vector3::new(-1.0, 0.0, 0.0), -max.x),
            (Vector3::new(0.0, 1.0, 0.0), min.y),
            (Vector3::new(0.0, -1.0, 0.0), -max.y),
            (Vector3::new(0.0, 0.0, 1.0), min.z),
            (Vector3::new(0.0, 0.0, -1.0), -max.z),
        ]
        .iter()
        .map(|(n, d)| ChunkBoundary {
            plane: PlaneEq::new(*n, *d),
            bound_portals: Vec::new(),
            unbound_portals: Vec::new(),
        })
        .collect();
        chunk.bounds = (0..6).collect();
        chunk.is_loaded = true;
        chunk
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

    /// A focussed chunk's obstacle reaches the column its corners touch,
    /// and a ray through it lands on the obstacle's BSP plane — the
    /// distance `ChunkSpace::collide` reports is the nearest triangle's
    /// distance along the sweep (`findDropPoint` consumes it).
    #[test]
    fn a_ray_through_a_focussed_obstacle_answers_the_nearest_triangle() {
        let mapping = identity_mapping();
        let mut space = ChunkSpace::new(0, vec![mapping.clone()]);
        add_outside_chunk(&mut space, &mapping, "00000000o");
        space.add_chunk(shell_chunk(&mapping));
        space.focus_chunk("00000000i");

        assert_eq!(space.add_obstacle(slab_obstacle(Some("00000000i"))), 0);

        let mut fdpt = ClosestTriangle::new();
        let dist = space.collide(
            Vector3::new(2.0, 2.0, 0.0),
            Vector3::new(2.0, 2.0, 8.0),
            &mut fdpt,
        );

        assert!((dist - 5.0).abs() < 1e-5, "dist {dist}");
        // the slab plane sits at z 5
        assert!((fdpt.triangle().v0().z - 5.0).abs() < 1e-5);
    }

    /// A callback that stops the sweep still reports the swept distance —
    /// the `ChunkSpace_collide` early-return arm.
    #[test]
    fn a_stopping_callback_returns_the_state_distance_early() {
        let mapping = identity_mapping();
        let mut space = ChunkSpace::new(0, vec![mapping.clone()]);
        add_outside_chunk(&mut space, &mapping, "00000000o");
        space.add_chunk(shell_chunk(&mapping));
        space.focus_chunk("00000000i");
        space.add_obstacle(slab_obstacle(Some("00000000i")));

        let dist = space.collide(
            Vector3::new(2.0, 2.0, 0.0),
            Vector3::new(2.0, 2.0, 8.0),
            &mut StoppingCallback,
        );
        assert!((dist - 5.0).abs() < 1e-5, "dist {dist}");
    }

    /// Outside the focus grid there is no column to walk, so the sweep
    /// never touches an obstacle and reports the untouched state distance.
    #[test]
    fn a_ray_outside_the_focus_grid_never_touches_an_obstacle() {
        let mapping = identity_mapping();
        let mut space = ChunkSpace::new(0, vec![mapping.clone()]);
        add_outside_chunk(&mut space, &mapping, "00000000o");
        space.add_chunk(shell_chunk(&mapping));
        space.focus_chunk("00000000i");
        space.add_obstacle(slab_obstacle(Some("00000000i")));

        let mut fdpt = ClosestTriangle::new();
        let dist = space.collide(
            Vector3::new(250.0, 2.0, 0.0),
            Vector3::new(250.0, 2.0, 8.0),
            &mut fdpt,
        );
        assert_eq!(dist, -1.0);
        assert_eq!(fdpt.triangle().v0(), Vector3::ZERO);
    }

    /// An obstacle whose chunk is not focussed — or that owns no chunk —
    /// never reaches a column: it sits in the arena only, and the sweep
    /// ignores it.
    #[test]
    fn an_unfocussed_chunk_obstacle_never_reaches_a_column() {
        let mapping = identity_mapping();
        let mut space = ChunkSpace::new(0, vec![mapping.clone()]);
        add_outside_chunk(&mut space, &mapping, "00000000o");
        space.add_chunk(shell_chunk(&mapping));

        // the shell was never focussed, and the outside chunk has no
        // focus either: both obstacles park in the arena only
        assert_eq!(space.add_obstacle(slab_obstacle(Some("00000000i"))), 0);
        assert_eq!(space.add_obstacle(slab_obstacle(Some("00000000o"))), 1);
        assert_eq!(space.add_obstacle(slab_obstacle(None)), 2);

        let mut fdpt = ClosestTriangle::new();
        let dist = space.collide(
            Vector3::new(2.0, 2.0, 0.0),
            Vector3::new(2.0, 2.0, 8.0),
            &mut fdpt,
        );
        assert_eq!(dist, -1.0);
    }
}
