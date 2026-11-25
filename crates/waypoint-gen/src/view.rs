//! The navmesh record stream, and the writer that builds it.
//!
//! `IWaypointView::asBinary` (`waypoint_view.cpp:308-404`) serialises a
//! waypoint view — the flood/BSP polygons or a Recast conversion — into
//! the record stream that lands in a chunk's `.cdata/worldNavmesh`
//! binary section. The NavGen workflow's save arm is this writer:
//! choose the target girths, generate polygons, hand them here.
//!
//! One record per nav poly set:
//!
//! ```text
//! int32  version         // always 0
//! float  girth
//! int32  numWaypoints    // control stream / 12
//! int32  numEdges        // data stream / 12
//! // control stream: minHeight, maxHeight, vertexCount per polygon
//! // data stream:    x, z, adjacency per vertex
//! ```
//!
//! The set map is the reference's `BW::map<int,int>` — ordered by set
//! index, so the records come out sorted.

use std::collections::BTreeMap;

pub const NAV_POLY_SET_ELT_SIZE: usize = 16;
pub const NAV_POLY_ELT_SIZE: usize = 12;
pub const NAV_POLY_EDGE_ELT_SIZE: usize = 12;

/// Adjacency value for a vertex that borders another chunk
/// (`waypoint_view.cpp:363`).
pub const ADJ_ANOTHER_CHUNK: i32 = 65535;

/// One vertex as the view reports it: world position already scaled by
/// `VertexRef::getVertex` (grid coords × gridResolution + gridMin,
/// `waypoint_view.hpp:110-118`) plus the raw adjacency field.
#[derive(Clone, Copy, Debug)]
pub struct NavVertex {
    pub x: f32,
    pub z: f32,
    pub adj_nav_poly: i32,
    pub adj_to_another_chunk: bool,
}

/// The polygon face the reference reads through: `PolygonRef`/
/// `VertexRef` (`waypoint_view.hpp:119-134`).
pub trait NavView {
    fn polygon_count(&self) -> i32;
    fn girth(&self, set: i32) -> f32;
    fn poly_set(&self, poly: i32) -> i32;
    fn min_height(&self, poly: i32) -> f32;
    fn max_height(&self, poly: i32) -> f32;
    fn vertex_count(&self, poly: i32) -> i32;
    /// `polygon[0].pos()` as `(x, y)`.
    fn first_vertex(&self, poly: i32) -> (f32, f32);
    fn vertex(&self, poly: i32, vertex: i32) -> NavVertex;
}

/// `PairOfStreams` (`waypoint_view.cpp:288-306`).
#[derive(Default)]
struct SetStream {
    ctrl: Vec<u8>,
    data: Vec<u8>,
    girth: f32,
    count: i32,
    /// 1-based polygon index → position within this set.
    index_map: BTreeMap<i32, i32>,
}

/// `IWaypointView::asBinary` with the processor's identity transform
/// (`navmesh_processor.cpp:96`).
pub fn as_binary(view: &dyn NavView, default_girth: f32) -> Vec<u8> {
    let mut sets: BTreeMap<i32, SetStream> = BTreeMap::new();

    // preflight polygon indices for each nav poly set
    for p in 0..view.polygon_count() {
        let set = view.poly_set(p);
        let streams = sets.entry(set).or_default();
        if streams.girth <= 0.0 {
            let girth = view.girth(set);
            streams.girth = if girth <= 0.0 { default_girth } else { girth };
        }
        // we are p+1 in adjacency field
        streams.index_map.insert(p + 1, streams.count);
        streams.count += 1;
    }

    // build the pairs of streams simultaneously for each polygon
    for p in 0..view.polygon_count() {
        let set = view.poly_set(p);
        let streams = sets.get_mut(&set).expect("set prefetched");
        let vertex_count = view.vertex_count(p);
        let (x0, z0) = view.first_vertex(p);
        let min_height = view.min_height(p);
        let max_height = view.max_height(p);

        // transformToLocal is identity, so the heights are the polygon's
        // own and only the first vertex's position matters
        let _ = (x0, z0);
        // control stream: mhv.y << Mhv.y << vertexCount
        streams.ctrl.extend_from_slice(&min_height.to_le_bytes());
        streams.ctrl.extend_from_slice(&max_height.to_le_bytes());
        streams.ctrl.extend_from_slice(&vertex_count.to_le_bytes());

        // data stream
        for v in 0..vertex_count {
            let vertex = view.vertex(p, v);
            streams.data.extend_from_slice(&vertex.x.to_le_bytes());
            streams.data.extend_from_slice(&vertex.z.to_le_bytes());

            let mut adjacency = vertex.adj_nav_poly;
            if adjacency > 0 && adjacency <= view.polygon_count() {
                // an adjacency into another set inserts a phantom entry
                // that default-initialises to 0 (std::map operator[])
                adjacency = *streams.index_map.get(&adjacency).unwrap_or(&0);
            } else if vertex.adj_to_another_chunk {
                adjacency = ADJ_ANOTHER_CHUNK;
            } else if adjacency <= 0 {
                // the reference writes `~ -adjacency`, which is a - 1
                adjacency -= 1;
            }
            streams.data.extend_from_slice(&adjacency.to_le_bytes());
        }
    }

    let mut out = Vec::new();
    for streams in sets.values() {
        out.extend_from_slice(&0i32.to_le_bytes()); // version 0
        out.extend_from_slice(&streams.girth.to_le_bytes());
        out.extend_from_slice(&((streams.ctrl.len() / NAV_POLY_ELT_SIZE) as i32).to_le_bytes());
        out.extend_from_slice(
            &((streams.data.len() / NAV_POLY_EDGE_ELT_SIZE) as i32).to_le_bytes(),
        );
        out.extend_from_slice(&streams.ctrl);
        out.extend_from_slice(&streams.data);
    }
    out
}

/// One parsed old record: `(start_offset, end_offset, girth)`.
struct OldPart {
    start: usize,
    end: usize,
    girth: f32,
}

/// Split an old navmesh record stream into its records
/// (`waypoint_view.cpp:197-218`).
fn parse_old_parts(old_navmesh: &[u8]) -> Vec<OldPart> {
    let mut parts = Vec::new();
    let mut off = 0usize;
    while off < old_navmesh.len() {
        let start = off;
        let girth = f32::from_le_bytes(old_navmesh[off + 4..off + 8].try_into().unwrap());
        let nav_poly_elts =
            i32::from_le_bytes(old_navmesh[off + 8..off + 12].try_into().unwrap()) as usize;
        let nav_poly_edges =
            i32::from_le_bytes(old_navmesh[off + 12..off + 16].try_into().unwrap()) as usize;
        off += 16 + nav_poly_elts * NAV_POLY_ELT_SIZE + nav_poly_edges * NAV_POLY_EDGE_ELT_SIZE;
        parts.push(OldPart {
            start,
            end: off,
            girth,
        });
    }
    parts
}

/// `IWaypointView::saveOut`'s merge arm (`waypoint_view.cpp:184-241`):
/// the new record stream is written first, then every old record whose
/// girth differs from `default_girth` is kept in its old order — the
/// default-girth record is superseded by the freshly generated one.
///
/// The reference marks dropped records by setting bit31 of the stored
/// offset (`offset |= (1<<31)`, `waypoint_view.cpp:211`) and re-tests it
/// with `oldParts[i] >> 31` in the append loop (`:232`); here the girth
/// test is done directly. The dev-build `MF_ASSERT(offset <= INT_MAX)`
/// at `:212` trips on any old default-girth record (the marked offset
/// exceeds INT_MAX); release builds skip it and that semantics is
/// ported.
pub fn save_out_records(
    new_navmesh: &[u8],
    old_navmesh: Option<&[u8]>,
    default_girth: f32,
    remove_all_old: bool,
) -> Vec<u8> {
    let old = match old_navmesh {
        Some(old) if !remove_all_old => old,
        _ => return new_navmesh.to_vec(),
    };

    let parts = parse_old_parts(old);
    let keep_size: usize = parts
        .iter()
        .filter(|part| part.girth != default_girth)
        .map(|part| part.end - part.start)
        .sum();

    if keep_size == 0 {
        return new_navmesh.to_vec();
    }

    let mut combo = Vec::with_capacity(new_navmesh.len() + keep_size);
    combo.extend_from_slice(new_navmesh);
    let mut cur_offset = 0usize;
    for part in &parts {
        if part.girth != default_girth {
            combo.extend_from_slice(&old[cur_offset..part.end]);
        }
        cur_offset = part.end;
    }
    combo
}

#[cfg(test)]
mod artifacts;
