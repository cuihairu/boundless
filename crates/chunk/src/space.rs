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
//! BW-10-01-04 adds the unload half: `unbind` ports `Chunk::unbind` in both
//! directions (portals walk back, targets cut to `INVASIVE`/`NULL`, internal
//! joints erased) and `unload_chunk` ports `ChunkSpace::unloadChunk` with its
//! `removable_`/`loaded_`/`isBound_` guards.
//!
//! Registered divergences (see `docs/review/bw10-01-chunk-load.md`): the
//! within-equal-radius order of the reference `std::sort` is unspecified, so
//! this port pins a deterministic tie-break (radius, then offset x, then z)
//! and the tests assert the radius order and cell set, not a brittle per-tier
//! sequence; `resolveExtern`/invasive portal resolution need the space column
//! (`guessChunk`) and `ChunkOverlappers` belong to the scene-adaptation
//! surface (BW-10-02); `gridToLocal` is the identity (transformed mappings
//! are BW-10-02+).

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
/// BW-10-01-05 adds the focus grid (`ServerChunkSpace::currentFocus_`) and
/// the blurred-chunk list it refocuses from.
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
        // `pSpace_->noticeChunk( this )` (`chunk.cpp:1072`): the chunk may
        // be focussed now
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

    /// `ChunkSpace::findChunkFromPoint` (`chunk_space.cpp:736-755`): the
    /// column-level query — outside chunk first, then larger hull volumes.
    pub fn find_chunk_from_point(&self, point: &Vector3) -> Option<String> {
        let mut pt = *point;
        pt.y += 0.0001;
        let column = self.column_at(&pt)?;
        column.find_chunk(&pt, &self.chunks)
    }

    /// `ChunkSpace::findChunkFromPointExact` (`chunk_space.cpp:757-768`):
    /// the query result only counts when the chunk is completed — loaded,
    /// bound and still focussed (`chunk.hpp:183,186`).
    pub fn find_chunk_from_point_exact(&self, point: &Vector3) -> Option<String> {
        let result = self.find_chunk_from_point(point)?;
        match self.chunk(&result) {
            Some(chunk) if chunk.completed() => Some(result),
            _ => None,
        }
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
    use crate::boundary::{ChunkBoundary, Portal, PortalTarget};
    use crate::chunk::ChunkItem;
    use crate::mapping::{GeometryMapping, GridBounds};
    use crate::obstacle::test_bsp::slab_obstacle;
    use foundation::math::PlaneEq;
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

    /// Two loaded chunks, each holding one unbound face portal at the
    /// other, plus a scene item each.
    fn pair_of_chunks() -> (Chunk, Chunk) {
        let item = |tag: &str| ChunkItem {
            tag: tag.to_string(),
            label: String::new(),
            resource: Some("thing.model".to_string()),
            position: None,
            transform: Matrix::identity(),
        };
        let face = |target: &str| ChunkBoundary {
            plane: PlaneEq::new(Vector3::new(1.0, 0.0, 0.0), 0.0),
            bound_portals: Vec::new(),
            unbound_portals: vec![face_portal(target)],
        };
        let mut a = Chunk::new("00000000o", &identity_mapping(), &Matrix::identity());
        a.boundaries = vec![face("00000001o")];
        a.joints = vec![0];
        a.items = vec![item("model")];
        a.is_loaded = true;
        let mut b = Chunk::new("00000001o", &identity_mapping(), &Matrix::identity());
        b.boundaries = vec![face("00000000o")];
        b.joints = vec![0];
        b.items = vec![item("light")];
        b.is_loaded = true;
        (a, b)
    }

    /// `ChunkSpace::unloadChunk` (`chunk_space.cpp:1262-1296`) unbinds
    /// (portals back to unbound on BOTH sides, targets preserved) and then
    /// releases the chunk's own state. The chunk stays appointed in the
    /// map, and a fresh load plus bind restores the exact baseline.
    #[test]
    fn unload_chunk_unbinds_then_releases_and_a_rebind_restores_the_baseline() {
        let mut space = ChunkSpace::new(0, vec![identity_mapping()]);
        let (a, b) = pair_of_chunks();
        space.add_chunk(a);
        space.add_chunk(b);
        assert!(space.bind("00000000o"));
        assert!(space.bind("00000001o"));
        assert_eq!(space.bind_order(), ["00000000o", "00000001o"]);

        assert!(space.unload_chunk("00000001o"));

        // The unloaded chunk: everything the reference releases
        // (chunk.cpp:914-980) is gone, but the object stays appointed.
        let b = space.chunk("00000001o").unwrap();
        assert!(!b.is_loaded);
        assert!(!b.is_bound);
        assert!(b.items.is_empty());
        assert!(b.joints.is_empty() && b.bounds.is_empty());
        assert!(b.boundaries.is_empty());
        assert!(space.chunk("00000001o").is_some(), "still appointed");

        // The neighbour keeps its own bind but the shared portal moved
        // back to unbound, target preserved (`unbindPortal` without cut).
        let a = space.chunk("00000000o").unwrap();
        assert!(a.is_bound);
        assert!(a.boundaries[0].bound_portals.is_empty());
        assert_eq!(
            a.boundaries[0].unbound_portals[0].target,
            PortalTarget::Chunk("00000001o".into())
        );

        // Already unloaded: false, and the map keeps the husk.
        assert!(!space.unload_chunk("00000001o"));

        // A fresh load plus bind restores the exact query baseline.
        let (a, b) = pair_of_chunks();
        let restored = space.chunks.get_mut("00000001o").unwrap();
        *restored = b;
        assert!(space.bind("00000001o"));
        assert_eq!(
            space.chunk("00000001o").unwrap().boundaries[0]
                .bound_portals
                .len(),
            1
        );
        assert_eq!(
            space.chunk("00000000o").unwrap().boundaries[0]
                .bound_portals
                .len(),
            1,
            "the two-way reconnect"
        );
        let _ = a; // the pairing helper's other half
    }

    /// With `cut` the disconnect cannot silently rebind: both sides'
    /// targets clear to `NULL` (outside to outside), so a later `bind`
    /// finds nothing to connect (`chunk.cpp:1179-1190`).
    #[test]
    fn a_cut_unbind_clears_the_targets_so_nothing_rebinds() {
        let mut space = ChunkSpace::new(0, vec![identity_mapping()]);
        let (a, b) = pair_of_chunks();
        space.add_chunk(a);
        space.add_chunk(b);
        assert!(space.bind("00000000o"));
        assert!(space.bind("00000001o"));

        space.unbind("00000001o", true);

        // The walking side moves its portal back (unbindPortal, target
        // cleared); the neighbour's back link stays BOUND with a cleared
        // target — the back arm only clears `pChunk`, it never moves the
        // portal (`chunk.cpp:2099-2105`, `2124-2126`).
        assert_eq!(
            space.chunk("00000001o").unwrap().boundaries[0].unbound_portals[0].target,
            PortalTarget::Nothing
        );
        assert_eq!(
            space.chunk("00000000o").unwrap().boundaries[0].bound_portals[0].target,
            PortalTarget::Nothing
        );

        // Rebinding connects nothing — the names are gone.
        assert!(space.bind("00000001o"));
        assert!(
            space.chunk("00000001o").unwrap().boundaries[0]
                .bound_portals
                .is_empty()
        );
    }

    /// An inside chunk cutting free of an outside neighbour marks its own
    /// moved portal `INVASIVE` (`chunk.cpp:1437-1438`); the outside
    /// neighbour's back link clears to `NULL`.
    #[test]
    fn an_inside_chunk_cutting_free_goes_invasive_the_outside_goes_null() {
        let mut space = ChunkSpace::new(0, vec![identity_mapping()]);
        // `b` faces the inside chunk, so binding the inside chunk two-way
        // connects (its `bind_back` finds a portal aimed at "00000000i").
        let mut b = Chunk::new("00000001o", &identity_mapping(), &Matrix::identity());
        b.boundaries = vec![ChunkBoundary {
            plane: PlaneEq::new(Vector3::new(1.0, 0.0, 0.0), 0.0),
            bound_portals: Vec::new(),
            unbound_portals: vec![face_portal("00000000i")],
        }];
        b.joints = vec![0];
        b.is_loaded = true;
        space.add_chunk(b);
        assert!(space.bind("00000001o"));

        let mut inside = Chunk::new("00000000i", &identity_mapping(), &Matrix::identity());
        inside.boundaries = vec![ChunkBoundary {
            plane: PlaneEq::new(Vector3::new(1.0, 0.0, 0.0), 0.0),
            bound_portals: Vec::new(),
            unbound_portals: vec![face_portal("00000001o")],
        }];
        inside.joints = vec![0];
        inside.is_loaded = true;
        space.add_chunk(inside);
        assert!(space.bind("00000000i"));

        space.unbind("00000000i", true);
        assert_eq!(
            space.chunk("00000000i").unwrap().boundaries[0].unbound_portals[0].target,
            PortalTarget::Invasive
        );
        // The outside neighbour's back link stays bound, target cleared.
        assert_eq!(
            space.chunk("00000001o").unwrap().boundaries[0].bound_portals[0].target,
            PortalTarget::Nothing
        );
    }

    /// `unbind` from an *outside* chunk forces the neighbour's cut
    /// (`chunk.cpp:1446-1450`): an exit portal into an inside chunk is always
    /// cut off, even when the caller asked for a plain unbind. The outside
    /// chunk's own portal keeps its target (`cut` false).
    #[test]
    fn an_outside_chunk_always_cuts_an_exit_portal_into_an_inside_neighbour() {
        let mut space = ChunkSpace::new(0, vec![identity_mapping()]);
        let face = |target: &str| ChunkBoundary {
            plane: PlaneEq::new(Vector3::new(1.0, 0.0, 0.0), 0.0),
            bound_portals: Vec::new(),
            unbound_portals: vec![face_portal(target)],
        };
        let mut inside = Chunk::new("00000000i", &identity_mapping(), &Matrix::identity());
        inside.boundaries = vec![face("00000000o")];
        inside.joints = vec![0];
        inside.is_loaded = true;
        space.add_chunk(inside);
        let mut outside = Chunk::new("00000000o", &identity_mapping(), &Matrix::identity());
        outside.boundaries = vec![face("00000000i")];
        outside.joints = vec![0];
        outside.is_loaded = true;
        space.add_chunk(outside);
        assert!(space.bind("00000000i"));
        assert!(space.bind("00000000o"));

        space.unbind("00000000o", false);

        // The outside chunk's portal moved back with its target intact.
        let boundary = &space.chunk("00000000o").unwrap().boundaries[0];
        assert!(boundary.bound_portals.is_empty());
        assert_eq!(
            boundary.unbound_portals[0].target,
            PortalTarget::Chunk("00000000i".into())
        );
        // The inside neighbour's back link was still cut: owner inside,
        // requester outside, so the cleared target is INVASIVE.
        assert_eq!(
            space.chunk("00000000i").unwrap().boundaries[0].bound_portals[0].target,
            PortalTarget::Invasive
        );
    }

    /// A heavenly portal in an inside chunk unbinds itself
    /// (`chunk.cpp:1420-1426`); in an outside chunk it has no chunk and
    /// stays bound.
    #[test]
    fn a_heavenly_portal_unbinds_only_from_an_inside_chunk() {
        let mut space = ChunkSpace::new(0, vec![identity_mapping()]);
        let mut inside = Chunk::new("00000000i", &identity_mapping(), &Matrix::identity());
        inside.boundaries = vec![ChunkBoundary {
            plane: PlaneEq::new(Vector3::new(1.0, 0.0, 0.0), 0.0),
            bound_portals: Vec::new(),
            unbound_portals: vec![face_portal("heaven")],
        }];
        inside.joints = vec![0];
        inside.is_loaded = true;
        space.add_chunk(inside);
        assert!(space.bind("00000000i"));
        space.unbind("00000000i", false);
        assert_eq!(
            space.chunk("00000000i").unwrap().boundaries[0]
                .unbound_portals
                .len(),
            1,
            "the heaven portal moved back"
        );
        assert!(
            space.chunk("00000000i").unwrap().boundaries[0]
                .bound_portals
                .is_empty()
        );

        let mut outside = Chunk::new("00000002o", &identity_mapping(), &Matrix::identity());
        outside.boundaries = vec![ChunkBoundary {
            plane: PlaneEq::new(Vector3::new(1.0, 0.0, 0.0), 0.0),
            bound_portals: Vec::new(),
            unbound_portals: vec![face_portal("heaven")],
        }];
        outside.joints = vec![0];
        outside.is_loaded = true;
        space.add_chunk(outside);
        assert!(space.bind("00000002o"));
        space.unbind("00000002o", false);
        assert_eq!(
            space.chunk("00000002o").unwrap().boundaries[0]
                .bound_portals
                .len(),
            1,
            "the heaven portal stays bound outside"
        );
    }

    /// A joint holds more portals than `unbind` moves: the reference walks
    /// the bound list and only takes the portals it decides to release
    /// (`chunk.cpp:1414-1451`), so an earth portal born bound sits through
    /// the unbind untouched while the heaven portal leaves the list.
    #[test]
    fn a_joint_keeps_the_portals_unbind_does_not_release() {
        let mut space = ChunkSpace::new(0, vec![identity_mapping()]);
        let mut inside = Chunk::new("00000000i", &identity_mapping(), &Matrix::identity());
        inside.boundaries = vec![ChunkBoundary {
            plane: PlaneEq::new(Vector3::new(1.0, 0.0, 0.0), 0.0),
            bound_portals: vec![face_portal("earth")],
            unbound_portals: vec![face_portal("heaven")],
        }];
        inside.joints = vec![0];
        inside.is_loaded = true;
        space.add_chunk(inside);
        assert!(space.bind("00000000i"));
        space.unbind("00000000i", false);

        let boundary = &space.chunk("00000000i").unwrap().boundaries[0];
        assert_eq!(
            boundary.bound_portals.len(),
            1,
            "the earth portal stayed bound"
        );
        assert_eq!(boundary.bound_portals[0].target, PortalTarget::Earth);
        assert_eq!(
            boundary.unbound_portals.len(),
            1,
            "the heaven portal moved back"
        );
        assert_eq!(boundary.unbound_portals[0].target, PortalTarget::Heaven);
    }

    /// The back-link arm erases the whole joint when the reverse portal is
    /// internal (`chunk.cpp:2107-2121`): the boundary leaves the joints
    /// list with its portal still bound, target cleared.
    #[test]
    fn a_cut_internal_back_link_erases_the_joint() {
        let mut space = ChunkSpace::new(0, vec![identity_mapping()]);
        let (a, mut b) = pair_of_chunks();
        space.add_chunk(a);
        b.boundaries[0].unbound_portals[0].internal = true;
        space.add_chunk(b);
        assert!(space.bind("00000000o"));
        assert!(space.bind("00000001o"));

        space.unbind("00000000o", true);

        let b = space.chunk("00000001o").unwrap();
        assert!(b.joints.is_empty(), "the internal joint left the list");
        assert_eq!(b.boundaries.len(), 1, "the boundary itself stays");
        assert_eq!(
            b.boundaries[0].bound_portals[0].target,
            PortalTarget::Nothing
        );
    }

    /// The unload guards: non-removable refuses untouched, not-loaded
    /// refuses, and `unloadChunks` answers exactly the loaded set in map
    /// order (`chunk_space.cpp:1269-1293`, `1204-1246`).
    #[test]
    fn the_unload_guards_refuse_and_unload_chunks_answers_the_loaded_set() {
        let mut space = ChunkSpace::new(0, vec![identity_mapping()]);
        let (a, mut b) = pair_of_chunks();
        space.add_chunk(a);
        b.removable = false;
        b.is_loaded = false;
        space.add_chunk(b);

        assert!(!space.unload_chunk("00000001o"), "non-removable");
        assert!(!space.unload_chunk("00000002o"), "not in the map");
        assert_eq!(
            space.chunk("00000001o").unwrap().items.len(),
            1,
            "the refusal leaves everything in place"
        );

        assert_eq!(space.unload_chunks(), vec!["00000000o".to_string()]);
        assert!(!space.chunk("00000000o").unwrap().is_loaded);
    }

    /// `Chunk::unload` itself (`chunk.cpp:914-980`): a still-bound chunk
    /// refuses (the reference errors out), an unloaded chunk is a no-op.
    #[test]
    fn a_direct_unload_refuses_bound_chunks_and_noops_unloaded_ones() {
        let mut space = ChunkSpace::new(0, vec![identity_mapping()]);
        let (mut a, _) = pair_of_chunks();
        space.add_chunk(a.clone());
        assert!(space.bind("00000000o"));
        a = space.chunks["00000000o"].clone();
        a.unload();
        assert!(a.is_loaded, "the bound refusal");
        assert_eq!(a.items.len(), 1);

        a.is_bound = false;
        a.unload();
        assert!(!a.is_loaded);
        assert!(a.items.is_empty());
        assert!(a.boundaries.is_empty());
        a.unload();
        assert!(!a.is_loaded, "the second unload is a no-op");
    }

    /// The port's guards for a missing neighbour: the reference's
    /// `Chunk::unbind(Chunk*, bool)` reports one with
    /// `ERROR_MSG("Chunk %s didn't find reverse portal to %s")`
    /// (`chunk.cpp:2136-2137`) and otherwise dereferences the pointer it was
    /// given, so a missing chunk would be a crash there. The port stays
    /// silent instead; registered.
    #[test]
    fn a_missing_neighbour_is_a_silent_no_op_on_both_arms() {
        let mut space = ChunkSpace::new(0, vec![identity_mapping()]);
        // Unbinding an unknown chunk is a no-op.
        space.unbind("99999999o", true);

        let face_boundary = |target: &str| ChunkBoundary {
            plane: PlaneEq::new(Vector3::new(1.0, 0.0, 0.0), 0.0),
            bound_portals: Vec::new(),
            unbound_portals: vec![face_portal(target)],
        };
        // `a` points at a chunk the space never got; the back call finds no
        // owner. `b` is bound but has no portal back to `a`.
        let mut a = Chunk::new("00000000o", &identity_mapping(), &Matrix::identity());
        a.boundaries = vec![face_boundary("99999999o")];
        a.joints = vec![0];
        a.is_loaded = true;
        a.is_bound = true;
        a.boundaries[0].bound_portals = std::mem::take(&mut a.boundaries[0].unbound_portals);
        space.add_chunk(a);

        let mut b = Chunk::new("00000001o", &identity_mapping(), &Matrix::identity());
        b.boundaries = vec![face_boundary("00000002o")];
        b.joints = vec![0];
        b.is_loaded = true;
        b.is_bound = true;
        b.boundaries[0].bound_portals = std::mem::take(&mut b.boundaries[0].unbound_portals);
        space.add_chunk(b);

        space.unbind("00000000o", true);
        space.unbind("00000001o", true);

        // Both unbound, and neither call touched a chunk that was absent.
        assert!(!space.chunk("00000000o").unwrap().is_bound);
        assert!(!space.chunk("00000001o").unwrap().is_bound);
        // The unbound portal moved back on each side.
        assert_eq!(
            space.chunk("00000000o").unwrap().boundaries[0]
                .unbound_portals
                .len(),
            1
        );
        assert_eq!(
            space.chunk("00000001o").unwrap().boundaries[0]
                .unbound_portals
                .len(),
            1
        );
    }

    // ------------------------------------------------------------------
    // BW-10-01-05: the focus grid and its point queries
    // ------------------------------------------------------------------

    fn grid_mapping(min_x: i32, min_z: i32, max_x: i32, max_z: i32) -> GeometryMapping {
        GeometryMapping::new(
            "space/",
            100.0,
            GridBounds {
                min_l_grid_x: min_x,
                min_l_grid_y: min_z,
                max_l_grid_x: max_x,
                max_l_grid_y: max_z,
                min_grid_x: min_x,
                min_grid_y: min_z,
                max_grid_x: max_x,
                max_grid_y: max_z,
            },
            false,
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

    /// BW-10-01-05 acceptance: cross-chunk and empty-region queries return
    /// the right sets — one answer per outside-chunk column, the shell hull
    /// inside it, and nothing where no chunk was focussed.
    #[test]
    fn the_focus_grid_answers_cross_chunk_and_empty_region_queries() {
        let mapping = identity_mapping();
        let mut space = ChunkSpace::new(0, vec![mapping.clone()]);
        assert_eq!(space.focus_span(), (0, 0, 2, 2));

        add_outside_chunk(&mut space, &mapping, "00000000o");
        add_outside_chunk(&mut space, &mapping, "00010000o");
        space.add_chunk(shell_chunk(&mapping));
        for id in ["00000000o", "00010000o", "00000000i"] {
            assert!(space.bind(id));
        }

        // nothing is findable before the blurred list is focussed
        assert_eq!(
            space.find_chunk_from_point(&Vector3::new(50.0, 0.0, 50.0)),
            None
        );
        assert_eq!(
            space.find_chunk_from_point_exact(&Vector3::new(50.0, 0.0, 50.0)),
            None
        );

        space.focus();
        assert!(space.blurred_chunks().is_empty());

        // cross-chunk: one answer per outside chunk's own column
        assert_eq!(
            space.find_chunk_from_point(&Vector3::new(50.0, 0.0, 50.0)),
            Some("00000000o".to_string())
        );
        assert_eq!(
            space.find_chunk_from_point(&Vector3::new(150.0, 0.0, 50.0)),
            Some("00010000o".to_string())
        );
        assert_eq!(
            space.find_outside_chunk_from_point(&Vector3::new(150.0, 0.0, 50.0)),
            Some("00010000o".to_string())
        );
        // inside the shell the hull result wins over the column's outside
        // chunk; elsewhere in the same column it does not
        assert_eq!(
            space.find_chunk_from_point(&Vector3::new(15.0, 5.0, 15.0)),
            Some("00000000i".to_string())
        );
        assert_eq!(
            space.find_chunk_from_point(&Vector3::new(90.0, 0.0, 90.0)),
            Some("00000000o".to_string())
        );
        // an in-span column no chunk was focussed into: the empty region
        assert_eq!(
            space.find_chunk_from_point(&Vector3::new(150.0, 0.0, 150.0)),
            None
        );
        assert_eq!(
            space.find_outside_chunk_from_point(&Vector3::new(150.0, 0.0, 150.0)),
            None
        );
        // beyond the mappings' grid bounds there is not even a column
        assert_eq!(
            space.find_chunk_from_point(&Vector3::new(250.0, 0.0, 50.0)),
            None
        );
        // completed gating: focussed chunks answer the exact query
        assert_eq!(
            space.find_chunk_from_point_exact(&Vector3::new(50.0, 0.0, 50.0)),
            Some("00000000o".to_string())
        );
        assert_eq!(
            space.find_chunk_from_point_exact(&Vector3::new(15.0, 5.0, 15.0)),
            Some("00000000i".to_string())
        );
    }

    /// Unbinding deletes the ignored chunk's columns, smudges the co-held
    /// neighbours onto the blurred list, and the next `focus` rebuilds them.
    #[test]
    fn unbinding_blurs_and_a_refocus_rebuilds_the_neighbourhood() {
        let mapping = identity_mapping();
        let mut space = ChunkSpace::new(0, vec![mapping.clone()]);
        add_outside_chunk(&mut space, &mapping, "00000000o");
        add_outside_chunk(&mut space, &mapping, "00010000o");
        assert!(space.bind("00000000o"));
        assert!(space.bind("00010000o"));
        space.focus();
        assert_eq!(
            space.find_chunk_from_point(&Vector3::new(50.0, 0.0, 50.0)),
            Some("00000000o".to_string())
        );

        space.unbind("00000000o", false);
        assert!(!space.chunk("00000000o").unwrap().completed());
        // the neighbour was smudged off its (deleted) column
        assert_eq!(
            space.blurred_chunks().first().map(String::as_str),
            Some("00010000o")
        );
        // and the ignored chunk's own column is gone
        assert_eq!(
            space.find_chunk_from_point(&Vector3::new(50.0, 0.0, 50.0)),
            None
        );

        // the smudged neighbour refocuses
        space.focus();
        assert!(space.blurred_chunks().is_empty());
        assert!(space.chunk("00010000o").unwrap().completed());
        assert_eq!(
            space.find_chunk_from_point(&Vector3::new(150.0, 0.0, 50.0)),
            Some("00010000o".to_string())
        );
    }

    /// `smudge` blurs a still-bound chunk without touching the columns: the
    /// plain query still answers from the stale hull while the exact one
    /// refuses the un-completed chunk, until the next focus rebuilds it.
    #[test]
    fn smudge_blurs_but_keeps_the_hull_findable() {
        let mapping = identity_mapping();
        let mut space = ChunkSpace::new(0, vec![mapping.clone()]);
        add_outside_chunk(&mut space, &mapping, "00000000o");
        assert!(space.bind("00000000o"));
        space.focus();
        assert_eq!(
            space.find_chunk_from_point_exact(&Vector3::new(50.0, 0.0, 50.0)),
            Some("00000000o".to_string())
        );

        space.smudge_chunk("00000000o");
        assert_eq!(
            space.find_chunk_from_point(&Vector3::new(50.0, 0.0, 50.0)),
            Some("00000000o".to_string())
        );
        assert_eq!(
            space.find_chunk_from_point_exact(&Vector3::new(50.0, 0.0, 50.0)),
            None
        );
        // a second smudge of an already-unfocussed chunk does nothing
        space.smudge_chunk("00000000o");
        assert_eq!(
            space.blurred_chunks().first().map(String::as_str),
            Some("00000000o")
        );
        // an unknown or unbound chunk never joins the blurred list
        space.blurred_chunk("no-such-chunk");
        assert_eq!(space.blurred_chunks().len(), 1);
        // a missing chunk is a no-op focus
        space.focus_chunk("no-such-chunk");

        space.focus();
        assert_eq!(
            space.find_chunk_from_point_exact(&Vector3::new(50.0, 0.0, 50.0)),
            Some("00000000o".to_string())
        );
    }

    /// The focus window follows the mappings' grid bounds; shrinking it
    /// drops the outside columns and smudges the chunks they held
    /// (`~Column` deletes the hull holdings, each of which smudges its
    /// chunk). A chunk centred at a negative cell exercises the manual
    /// negative adjust of the reference's centre-to-grid arithmetic.
    #[test]
    fn shrinking_the_grid_drops_and_smudges() {
        let mapping = grid_mapping(-1, 0, 0, 0);
        let mut space = ChunkSpace::new(0, vec![mapping.clone()]);
        assert_eq!(space.focus_span(), (-1, 0, 2, 1));

        let id = mapping.outside_chunk_identifier(-1, 0, false);
        add_outside_chunk(&mut space, &mapping, &id);
        assert!(space.bind(&id));
        space.focus();

        // the chunk centred at (-50, ·, 50) lives in cell (-1, 0)
        assert_eq!(
            space.find_chunk_from_point(&Vector3::new(-50.0, 0.0, 50.0)),
            Some(id.clone())
        );

        // shrink the window to cell (0,0): the chunk's column drops and
        // smudges it onto the blurred list
        space.mappings[0] = grid_mapping(0, 0, 0, 0);
        space.recalc_grid_bounds();
        assert_eq!(space.focus_span(), (0, 0, 1, 1));
        assert_eq!(
            space.blurred_chunks().first().map(String::as_str),
            Some(id.as_str())
        );
        assert_eq!(
            space.find_chunk_from_point(&Vector3::new(-50.0, 0.0, 50.0)),
            None
        );
    }

    /// `column(pt, true)` creates the column when willing and able; out of
    /// the focus span it always refuses. An empty mapping set still sizes a
    /// 1×1 window.
    #[test]
    fn column_creates_on_demand_and_refuses_out_of_span() {
        let mut space = ChunkSpace::new(0, vec![identity_mapping()]);
        assert!(space.column(&Vector3::new(10.0, 0.0, 10.0), true).is_some());
        assert!(
            space
                .column(&Vector3::new(10.0, 0.0, 10.0), false)
                .is_some()
        );
        assert!(
            space
                .column(&Vector3::new(250.0, 0.0, 10.0), true)
                .is_none()
        );

        let empty = ChunkSpace::new(9, vec![]);
        assert_eq!(empty.focus_span(), (0, 0, 1, 1));
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
