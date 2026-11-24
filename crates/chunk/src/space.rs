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

use std::collections::BTreeMap;

use foundation::math::{BoundBox, Matrix, Vector3};
use resources::datasection::DataSection;

use crate::boundary::PortalTarget;
use crate::chunk::Chunk;
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
}

impl ChunkSpace {
    pub fn new(id: u16, mappings: Vec<GeometryMapping>) -> Self {
        Self {
            id,
            mappings,
            chunks: BTreeMap::new(),
            bind_order: Vec::new(),
        }
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
        let chunk = self.chunks.get_mut(identifier).unwrap();
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
}
