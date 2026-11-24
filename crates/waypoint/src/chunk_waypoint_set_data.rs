//! `lib/waypoint/chunk_waypoint_set_data.{hpp,cpp}` — the per-set waypoint
//! storage and the version 0 `worldNavmesh` decode.
//!
//! `readWaypointSet` walks two cursors through the record: the waypoint
//! headers (`NAVPOLY_ELEMENT_SIZE` each: two floats and a vertex count)
//! first, then the edge data stream (`float x, float y, int32 neighbour`
//! records — "'adj' already encoded as we like it"). The reference reads
//! through raw `const char*` casts with no bounds checks; the port reports
//! truncation instead (the same registered divergence as the physics BSP
//! decode).

use super::chunk_waypoint::{ChunkWaypoint, ChunkWaypointVertexProvider, Edge};
use super::{EdgeIndex, WaypointIndex};
use foundation::math::{Vector2, Vector3};
use std::collections::HashMap;

/// `NAVPOLY_ELEMENT_SIZE` (`chunk_waypoint_set_data.cpp:437-438`):
/// `sizeof(float) + sizeof(float) + sizeof(int32)`.
pub const NAVPOLY_ELEMENT_SIZE: usize = 12;

/// The decode errors. The reference walks raw pointers with no bounds
/// checks; the port reports truncation (`readWaypointSet` /
/// `navmeshFactory`, registered divergence).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NavmeshDecodeError {
    /// The record ends before the declared waypoint/edge counts fit.
    Truncated,
}

impl std::fmt::Display for NavmeshDecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            NavmeshDecodeError::Truncated => {
                f.write_str("Registered divergence: the reference reads out of bounds here.")
            }
        }
    }
}

impl std::error::Error for NavmeshDecodeError {}

/// `consume<C>` (`chunk_waypoint_set_data.cpp:446-455`): one little-endian
/// scalar off the cursor.
fn consume<'a>(data: &mut &'a [u8], size: usize) -> Result<&'a [u8], NavmeshDecodeError> {
    if data.len() < size {
        return Err(NavmeshDecodeError::Truncated);
    }
    let (head, tail) = data.split_at(size);
    *data = tail;
    Ok(head)
}

fn consume_f32(data: &mut &[u8]) -> Result<f32, NavmeshDecodeError> {
    let head = consume(data, 4)?;
    Ok(f32::from_le_bytes(head.try_into().unwrap()))
}

fn consume_i32(data: &mut &[u8]) -> Result<i32, NavmeshDecodeError> {
    let head = consume(data, 4)?;
    Ok(i32::from_le_bytes(head.try_into().unwrap()))
}

/// `VertexIndices` (`chunk_waypoint_set_data.cpp:462-530`): the mapping
/// between unique `Vector2`s and the vertex indices the edge data stores.
/// The reference hashes the raw 8 bytes of the vector (`hash_string`) while
/// equality floats-compare; keying on the bit patterns reproduces the
/// hashing behavior (-0.0 and 0.0 hash differently, so they stay distinct
/// despite comparing equal).
struct VertexIndices {
    vertices: Vec<Vector2>,
    map: HashMap<u64, EdgeIndex>,
}

impl VertexIndices {
    /// Rehashes for the declared edge count ("assuming load factor 1.0").
    fn new(num_edges: usize) -> Self {
        Self {
            vertices: Vec::with_capacity(num_edges),
            map: HashMap::with_capacity(num_edges),
        }
    }

    /// `getIndexForVertex` (`chunk_waypoint_set_data.cpp:484-517`): dedup
    /// or append; overflowing the `EdgeIndex` storage prints the CRITICAL
    /// message and continues with the wrapped index (the reference does not
    /// abort here).
    fn get_index_for_vertex(&mut self, vertex: Vector2) -> EdgeIndex {
        let key = (vertex.x.to_bits() as u64) << 32 | vertex.y.to_bits() as u64;
        if let Some(&index) = self.map.get(&key) {
            return index;
        }

        let index = self.vertices.len() as EdgeIndex;
        self.vertices.push(vertex);
        self.map.insert(key, index);

        const MAX_SIZE: usize = (1 << (EdgeIndex::BITS as usize)) - 1;
        if self.vertices.len() > MAX_SIZE {
            // CRITICAL_MSG("Chunk waypoint vertex indices have overflowed:
            // %zu > %zu\n") — printed, execution continues.
            eprintln!(
                "CRITICAL_MSG: Chunk waypoint vertex indices have overflowed: {} > {}",
                self.vertices.len(),
                MAX_SIZE
            );
        }

        index
    }

    fn into_vertices(self) -> Vec<Vector2> {
        self.vertices
    }
}

impl ChunkWaypointVertexProvider for VertexIndices {
    fn vertex_by_index(&self, index: EdgeIndex) -> &Vector2 {
        assert!(
            (index as usize) < self.vertices.len(),
            "reference MF_ASSERT"
        );
        &self.vertices[index as usize]
    }
}

/// `ChunkWaypointSetData` (`chunk_waypoint_set_data.hpp:39-69`): the data
/// read in from a waypoint set, possibly shared between chunks.
#[derive(Debug, Default)]
pub struct ChunkWaypointSetData {
    /// `girth_`.
    girth: f32,
    /// `waypoints_`.
    waypoints: Vec<ChunkWaypoint>,
    /// `edges_`: the shared edge array the waypoints slice into.
    edges: Vec<Edge>,
    /// `numEdges_` (the allocation size, only re-reported at destruction).
    num_edges: usize,
    /// `baseIndex_`: the set's first waypoint id in the whole-navmesh
    /// numbering ("yes, it starts from 1").
    base_index: WaypointIndex,
    /// `vertices_`.
    vertices: Vec<Vector2>,
}

impl ChunkWaypointSetData {
    /// `ChunkWaypointSetData(baseIndex)` (`chunk_waypoint_set_data.cpp:546-558`).
    pub fn new(base_index: WaypointIndex) -> Self {
        Self {
            girth: 0.0,
            base_index,
            ..Default::default()
        }
    }

    /// `girth()` (`chunk_waypoint_set_data.hpp:53-54`).
    pub fn girth(&self) -> f32 {
        self.girth
    }

    /// `girth(girth)` (`chunk_waypoint_set_data.hpp:56-57`).
    pub fn set_girth(&mut self, girth: f32) {
        self.girth = girth;
    }

    /// `waypoints()` (`chunk_waypoint_set_data.hpp:62-63`).
    pub fn waypoints(&self) -> &[ChunkWaypoint] {
        &self.waypoints
    }

    /// `baseIndex_` (set at construction, consumed by the set id mapping).
    pub fn base_index(&self) -> WaypointIndex {
        self.base_index
    }

    /// The set's deduplicated vertex table (`vertices_`).
    pub fn vertices(&self) -> &[Vector2] {
        &self.vertices
    }

    /// The set's shared edge array (`edges_`).
    pub fn edges(&self) -> &[Edge] {
        &self.edges
    }

    /// `ChunkWaypointSetData::find` (`chunk_waypoint_set_data.cpp:579-624`,
    /// re-exported by `ChunkWaypointSet::find`): the waypoint containing
    /// the point. With `ignore_height` set the height range is skipped and
    /// the best height fit wins instead — the first waypoint whose range
    /// straddles the point's height answers straight away.
    pub fn find(&self, point: Vector3, ignore_height: bool) -> WaypointIndex {
        let mut best_waypoint = -1;
        let mut best_height_diff = f32::MAX;

        for i in 0..self.waypoints.len() {
            let wp = &self.waypoints[i];
            if ignore_height {
                if wp.contains_projection(&self.edges, self, point) {
                    if point.y > wp.min_height - 0.1 && point.y < wp.max_height + 0.1 {
                        return i as WaypointIndex;
                    } else {
                        // find the best fit
                        let wp_avg_diff = (point.y - (wp.max_height + wp.min_height) / 2.0).abs();
                        if best_height_diff > wp_avg_diff {
                            best_height_diff = wp_avg_diff;
                            best_waypoint = i as WaypointIndex;
                        }
                    }
                }
            } else if wp.contains(&self.edges, self, point) {
                return i as WaypointIndex;
            }
        }
        best_waypoint
    }

    /// `ChunkWaypointSetData::find(chunk, point, bestDistanceSquared)`
    /// (`chunk_waypoint_set_data.cpp:626-647`): the closest waypoint, with
    /// `bestDistanceSquared` handed in as the search bound and handed back
    /// as the winner's distance. The reference measures the distance with
    /// `ChunkWaypoint::distanceSquared` — the point clipped into the
    /// polygon, its height clamped into the min/max band.
    pub fn find_closest(&self, point: Vector3, best_distance_squared: &mut f32) -> WaypointIndex {
        let mut best_waypoint = -1;
        for (i, wp) in self.waypoints.iter().enumerate() {
            let distance_squared = wp.distance_squared(&self.edges, self, point);
            if *best_distance_squared > distance_squared {
                *best_distance_squared = distance_squared;
                best_waypoint = i as WaypointIndex;
            }
        }
        best_waypoint
    }

    /// A waypoint's `DependentArray` slice of the shared edge array.
    pub fn waypoint_edges(&self, waypoint: usize) -> &[Edge] {
        &self.edges[self.waypoints[waypoint].edge_range()]
    }

    /// `getAbsoluteEdgeIndex` (`chunk_waypoint_set_data.cpp:663-672`): the
    /// edge's offset in the shared array (`&edge - edges_`), asserted to
    /// fit an `EdgeIndex`.
    pub fn absolute_edge_index(&self, waypoint: usize, edge_in_waypoint: usize) -> EdgeIndex {
        let wp = &self.waypoints[waypoint];
        assert!(
            edge_in_waypoint < wp.edge_count as usize,
            "reference indexes the waypoint's edge run"
        );
        let index = wp.edge_start as usize + edge_in_waypoint;
        assert!(
            index <= u16::MAX as usize,
            "reference MF_ASSERTs the absolute edge index <= USHRT_MAX"
        );
        index as EdgeIndex
    }

    /// `readWaypointSet` (`chunk_waypoint_set_data.cpp:677-743`): decode
    /// one waypoint set from the two-cursor record, returning the cursor
    /// past the consumed edge stream.
    pub fn read_waypoint_set<'a>(
        &mut self,
        data: &'a [u8],
        num_waypoints: usize,
        num_edges: usize,
    ) -> Result<&'a [u8], NavmeshDecodeError> {
        // The reference computes the edge-stream cursor with blind pointer
        // arithmetic (`pEdgeData = pData + numWaypoints *
        // NAVPOLY_ELEMENT_SIZE`); the port guards that one computation and
        // lets every consume report truncation past it.
        let headers = num_waypoints * NAVPOLY_ELEMENT_SIZE;
        if data.len() < headers {
            return Err(NavmeshDecodeError::Truncated);
        }

        let mut vertex_indices = VertexIndices::new(num_edges);
        let mut edges = vec![Edge::default(); num_edges];
        let mut waypoints: Vec<ChunkWaypoint> = Vec::with_capacity(num_waypoints);

        let mut ctrl = data;
        // pEdgeData = pData + numWaypoints * NAVPOLY_ELEMENT_SIZE
        let mut edge_data = &data[headers..];
        let mut num_edges_left = num_edges as i64;
        // the `nedge` allocation cursor
        let mut nedge = 0usize;

        for _p in 0..num_waypoints {
            let min_height = consume_f32(&mut ctrl)?;
            let max_height = consume_f32(&mut ctrl)?;
            let vertex_count = consume_i32(&mut ctrl)? as usize;

            // The reference walks its `edges_` allocation past the end when
            // the waypoint vertex counts overrun the declared edge count
            // (UB), tripping MF_ASSERT(numEdges == 0) afterwards; the port
            // refuses before the write.
            if nedge + vertex_count > num_edges {
                panic!("waypoint vertex counts overrun the declared edge count");
            }

            let mut wp = ChunkWaypoint {
                min_height,
                max_height,
                // edges_( nedge, nedge+vertexCount )
                edge_start: nedge as u32,
                edge_count: vertex_count as u32,
                ..Default::default()
            };
            nedge += vertex_count;

            for e in 0..vertex_count {
                let x = consume_f32(&mut edge_data)?;
                let y = consume_f32(&mut edge_data)?;
                let neighbour = consume_i32(&mut edge_data)? as EdgeIndex; // 'adj' already encoded as we like it

                let vertex_index = vertex_indices.get_index_for_vertex(Vector2::new(x, y));
                edges[wp.edge_start as usize + e] = Edge {
                    vertex_index,
                    neighbour,
                };
                num_edges_left -= 1;
            }

            wp.calc_centre(&edges, &vertex_indices);
            wp.visited.set(0);
            waypoints.push(wp);
        }

        // MF_ASSERT( numEdges == 0 )
        assert!(
            num_edges_left == 0,
            "reference MF_ASSERTs numEdges == 0 after decoding the set"
        );

        self.waypoints = waypoints;
        self.vertices = vertex_indices.into_vertices();
        self.edges = edges;
        self.num_edges = num_edges;
        Ok(edge_data)
    }
}

impl ChunkWaypointVertexProvider for ChunkWaypointSetData {
    /// `vertexByIndex` (`chunk_waypoint_set_data.hpp:48-51`).
    fn vertex_by_index(&self, index: EdgeIndex) -> &Vector2 {
        assert!(
            (index as usize) < self.vertices.len(),
            "reference MF_ASSERT"
        );
        &self.vertices[index as usize]
    }
}

/// `navmeshFactory`'s decode loop (`chunk_waypoint_set_data.cpp:755-800`),
/// minus the chunk mounting and the population registry: a stream of
/// version-0 set records, each `int32 version, float girth, int32
/// numWaypoints, int32 numEdges` followed by the set body. The set id
/// numbering starts from 1 and accumulates across the sets ("yes, it
/// starts from 1"). An empty stream decodes to no sets (the empty-navmesh
/// `SucceededWithoutItem` branch).
pub fn parse_world_navmesh(data: &[u8]) -> Result<Vec<ChunkWaypointSetData>, NavmeshDecodeError> {
    let mut data = data;
    let mut index: WaypointIndex = 1; // yes, it starts from 1
    let mut sets = Vec::new();

    while !data.is_empty() {
        let version = consume_i32(&mut data)?;
        let girth = consume_f32(&mut data)?;
        let num_waypoints = consume_i32(&mut data)? as usize;
        let num_edges = consume_i32(&mut data)? as usize;

        // MF_ASSERT( aVersion == 0 )
        assert!(
            version == 0,
            "reference MF_ASSERTs the navmesh version is 0"
        );

        let mut set = ChunkWaypointSetData::new(index);
        set.set_girth(girth);
        data = set.read_waypoint_set(data, num_waypoints, num_edges)?;
        sets.push(set);

        index += num_waypoints as WaypointIndex;
    }

    Ok(sets)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn put_f(b: &mut Vec<u8>, f: f32) {
        b.extend_from_slice(&f.to_le_bytes());
    }
    fn put_i(b: &mut Vec<u8>, i: i32) {
        b.extend_from_slice(&i.to_le_bytes());
    }

    /// A two-waypoint set sharing one vertex: triangle (0,0)-(2,0)-(1,2)
    /// and segment (2,0)-(3,1). Edges: waypoint 0 links 1, waypoint 1 is
    /// the chunk boundary 65535, one vista encoding.
    fn sample_set() -> Vec<u8> {
        let mut b = Vec::new();
        put_i(&mut b, 0); // version
        put_f(&mut b, 0.6); // girth
        put_i(&mut b, 2); // numWaypoints
        put_i(&mut b, 5); // numEdges
        // waypoint headers
        put_f(&mut b, 0.0);
        put_f(&mut b, 2.0);
        put_i(&mut b, 3);
        put_f(&mut b, 1.0);
        put_f(&mut b, 1.0);
        put_i(&mut b, 2);
        // edge stream: wp0 (0,0)->wp1, (2,0)->boundary, (1,2)->vista;
        // wp1 (2,0)->wp0, (3,1)->boundary
        let edges: [(f32, f32, i32); 5] = [
            (0.0, 0.0, 1),
            (2.0, 0.0, 65535),
            (1.0, 2.0, -5),
            (2.0, 0.0, 0),
            (3.0, 1.0, 65535),
        ];
        for (x, y, n) in edges {
            put_f(&mut b, x);
            put_f(&mut b, y);
            put_i(&mut b, n);
        }
        b
    }

    #[test]
    fn read_waypoint_set_decodes_the_two_cursor_stream() {
        let mut set = ChunkWaypointSetData::new(1);
        let blob = sample_set();
        let rest = set
            .read_waypoint_set(&blob[16..], 2, 5)
            .expect("the sample record is complete");

        assert!(rest.is_empty(), "the edge stream ends the record");
        assert_eq!(set.girth(), 0.0, "girth is set by the caller, not here");
        set.set_girth(0.6);
        assert_eq!(set.girth(), 0.6);
        assert_eq!(set.base_index(), 1);

        let wps = set.waypoints();
        assert_eq!(wps.len(), 2);
        assert_eq!(wps[0].min_height, 0.0);
        assert_eq!(wps[0].max_height, 2.0);
        assert_eq!(wps[0].edge_start, 0);
        assert_eq!(wps[0].edge_count, 3);
        assert_eq!(wps[1].edge_start, 3);
        assert_eq!(wps[1].edge_count, 2);
        assert_eq!(wps[0].visited.get(), 0);
        assert_eq!(wps[1].visited.get(), 0);

        // The shared edge array holds every edge in decode order and the
        // waypoint slices index into it (the DependentArray face).
        assert_eq!(set.edges().len(), 5);
        assert_eq!(set.waypoint_edges(0).len(), 3);
        assert_eq!(set.waypoint_edges(1).len(), 2);
        assert_eq!(set.absolute_edge_index(1, 1), 4);
        assert_eq!(set.absolute_edge_index(0, 2), 2);
        assert!(
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                set.absolute_edge_index(0, 3)
            }))
            .is_err(),
            "the run bounds the local index"
        );

        // Vertex dedup: (2,0) is shared by both waypoints, so five edges
        // resolve to four unique vertices.
        assert_eq!(set.vertices().len(), 4);
        assert_eq!(*set.vertex_by_index(0), Vector2::new(0.0, 0.0));
        assert_eq!(*set.vertex_by_index(1), Vector2::new(2.0, 0.0));
        assert_eq!(*set.vertex_by_index(3), Vector2::new(3.0, 1.0));

        // Adjacency decode: the in-set link, the boundary encoding and the
        // vista encoding all come back through the edge accessors.
        let e0 = &set.waypoint_edges(0)[0];
        assert_eq!(e0.neighbouring_waypoint(), 1);
        assert!(!e0.adjacent_to_chunk());
        let e1 = &set.waypoint_edges(0)[1];
        assert_eq!(e1.neighbouring_waypoint(), -1);
        assert!(e1.adjacent_to_chunk());
        let vista = &set.waypoint_edges(0)[2];
        assert_eq!(vista.neighbour, (-5i32) as u16, "~-5 = 0xFFFA");
        assert_eq!(vista.neighbouring_waypoint(), -1);
        assert!(vista.adjacent_to_chunk());
    }

    #[test]
    fn truncated_records_are_reported_not_read_out_of_bounds() {
        let blob = sample_set();
        // Header promises five edges; cut the record short.
        let mut set = ChunkWaypointSetData::new(1);
        let cases = [
            (&blob[16..16 + 12], "only one waypoint header left"),
            (&blob[16..16 + 3 * 12], "headers only"),
            (&blob[16..blob.len() - 12], "one edge record short"),
            (
                &blob[16..16 + 3 * 12 + 2 * 12],
                "edge stream cut mid-record",
            ),
            (&[], "an empty record"),
        ];
        for (data, name) in cases {
            let result = set.read_waypoint_set(data, 2, 5);
            assert_eq!(result, Err(NavmeshDecodeError::Truncated), "{name}");
        }
    }

    #[test]
    fn the_decode_error_reports_the_registered_divergence() {
        // An empty record is shorter than the declared waypoint headers,
        // which is exactly where the reference's blind pointer arithmetic
        // would run off the end of the buffer.
        let mut set = ChunkWaypointSetData::new(1);
        let err = set.read_waypoint_set(&[], 2, 5).unwrap_err();
        assert_eq!(err, NavmeshDecodeError::Truncated);
        assert_eq!(
            err.to_string(),
            "Registered divergence: the reference reads out of bounds here."
        );
    }

    #[test]
    fn the_waypoint_and_edge_counts_must_balance() {
        let blob = sample_set();
        // numEdges declared smaller than the vertex counts carry: the port
        // refuses the allocation overrun (the reference walks past its
        // edges_ allocation here and trips MF_ASSERT(numEdges == 0)
        // afterwards).
        let overrun = std::panic::catch_unwind(|| {
            let mut set = ChunkWaypointSetData::new(1);
            let _ = set.read_waypoint_set(&blob[16..], 2, 4);
        });
        assert!(overrun.is_err(), "reference MF_ASSERTs numEdges == 0");

        // numEdges declared larger than the stream decodes: every record
        // consumes cleanly, so MF_ASSERT(numEdges == 0) fires instead.
        let short = std::panic::catch_unwind(|| {
            let mut set = ChunkWaypointSetData::new(1);
            let _ = set.read_waypoint_set(&blob[16..], 2, 6);
        });
        assert!(short.is_err(), "reference MF_ASSERTs numEdges == 0");
    }

    #[test]
    fn the_height_ignoring_search_answers_the_best_height_fit() {
        // Two waypoints sharing the same x-z footprint at different heights
        // and neither straddling the searched range: the best-fit branch
        // answers the nearer average height, which is what the multi-layer
        // locate keys on.
        let mut b = Vec::new();
        let put_f = |b: &mut Vec<u8>, v: f32| b.extend_from_slice(&v.to_le_bytes());
        let put_i = |b: &mut Vec<u8>, v: i32| b.extend_from_slice(&v.to_le_bytes());
        put_i(&mut b, 0);
        put_f(&mut b, 0.6);
        put_i(&mut b, 2);
        put_i(&mut b, 8);
        put_f(&mut b, 0.0);
        put_f(&mut b, 1.0);
        put_i(&mut b, 4);
        put_f(&mut b, 3.0);
        put_f(&mut b, 4.0);
        put_i(&mut b, 4);
        for _ in 0..2 {
            for (x, y) in [(0.0f32, 0.0), (0.0, 2.0), (2.0, 2.0), (2.0, 0.0)] {
                put_f(&mut b, x);
                put_f(&mut b, y);
                put_i(&mut b, 65535);
            }
        }
        let mut sets = parse_world_navmesh(&b).expect("complete");
        let set = sets.remove(0);

        // Both projections contain the point and neither band straddles the
        // searched height: the nearer average height wins, so a point above
        // both layers answers the upper waypoint.
        assert_eq!(set.find(Vector3::new(1.0, 10.0, 1.0), true), 1);
        // ... and the lower average wins for a point below both.
        assert_eq!(set.find(Vector3::new(1.0, -10.0, 1.0), true), 0);
        // The upper band straddles, so it answers straight away.
        assert_eq!(set.find(Vector3::new(1.0, 3.5, 1.0), true), 1);
        // Nothing projects onto a point off the shared footprint.
        assert_eq!(set.find(Vector3::new(9.0, 10.0, 9.0), true), -1);
        // The height-honouring search answers the layer it is in and misses
        // outside both bands.
        assert_eq!(set.find(Vector3::new(1.0, 0.5, 1.0), false), 0);
        assert_eq!(set.find(Vector3::new(1.0, 3.5, 1.0), false), 1);
        assert_eq!(set.find(Vector3::new(1.0, 2.0, 1.0), false), -1);
    }

    #[test]
    fn the_vertex_provider_asserts_out_of_range_indices() {
        let mut set = ChunkWaypointSetData::new(1);
        let blob = sample_set();
        set.read_waypoint_set(&blob[16..], 2, 5).expect("complete");
        assert_eq!(*set.vertex_by_index(2), Vector2::new(1.0, 2.0));
        assert!(
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| { set.vertex_by_index(4) }))
                .is_err()
        );
    }

    #[test]
    fn parse_world_navmesh_walks_the_set_records() {
        // Two records: the sample set and a one-waypoint follower.
        let mut b = sample_set();
        put_i(&mut b, 0);
        put_f(&mut b, 1.0);
        put_i(&mut b, 1);
        put_i(&mut b, 2);
        put_f(&mut b, 5.0);
        put_f(&mut b, 6.0);
        put_i(&mut b, 2);
        put_f(&mut b, 20.0);
        put_f(&mut b, 20.0);
        put_i(&mut b, 3);
        put_f(&mut b, 21.0);
        put_f(&mut b, 20.0);
        put_i(&mut b, 65535);

        let sets = parse_world_navmesh(&b).expect("complete stream");
        assert_eq!(sets.len(), 2);
        // The base index starts from 1 and accumulates per set.
        assert_eq!(sets[0].base_index(), 1);
        assert_eq!(sets[1].base_index(), 3);
        assert_eq!(sets[0].girth(), 0.6);
        assert_eq!(sets[1].girth(), 1.0);
        assert_eq!(sets[1].waypoints().len(), 1);
        assert_eq!(sets[1].vertices().len(), 2, "vertices restart per set");

        // An empty stream answers no sets (the empty-navmesh branch).
        assert!(parse_world_navmesh(&[]).unwrap().is_empty());

        // A header cut short reports truncation.
        assert!(matches!(
            parse_world_navmesh(&b[..8]),
            Err(NavmeshDecodeError::Truncated)
        ));
    }

    #[test]
    fn a_non_zero_navmesh_version_asserts() {
        let mut b = Vec::new();
        put_i(&mut b, 1); // version
        put_f(&mut b, 0.6);
        put_i(&mut b, 0);
        put_i(&mut b, 0);
        let result = std::panic::catch_unwind(|| parse_world_navmesh(&b));
        assert!(result.is_err(), "reference MF_ASSERTs the version is 0");
    }

    #[test]
    fn the_vertex_index_overflow_prints_and_continues() {
        // 65537 unique vertices: the 65537th wraps its EdgeIndex to 0 and
        // the CRITICAL message prints without aborting.
        let unique = 65537usize;
        let mut b = Vec::new();
        put_i(&mut b, 0);
        put_f(&mut b, 0.6);
        put_i(&mut b, 1); // one waypoint
        put_i(&mut b, unique as i32);
        put_f(&mut b, 0.0);
        put_f(&mut b, 1.0);
        put_i(&mut b, unique as i32); // vertexCount == numEdges
        for i in 0..unique {
            let x = i as f32;
            put_f(&mut b, x);
            put_f(&mut b, 0.0);
            put_i(&mut b, 0);
        }

        let sets = parse_world_navmesh(&b).expect("the record decodes");
        let wp = &sets[0].waypoints()[0];
        assert_eq!(wp.edge_count as usize, unique);
        assert_eq!(sets[0].vertices().len(), unique);
        // The last vertex wrapped to index 0; its predecessor kept 65535.
        assert_eq!(sets[0].edges()[unique - 1].vertex_index, 0);
        assert_eq!(sets[0].edges()[unique - 2].vertex_index, 65535);
    }

    #[test]
    fn the_closest_search_measures_the_clipped_point() {
        let blob = sample_set();
        let mut set = ChunkWaypointSetData::new(1);
        set.read_waypoint_set(&blob[16..], 2, 5)
            .expect("the sample decodes");
        set.set_girth(0.6);

        // The reference measures the clipped point: (1,1) sits outside
        // waypoint 0's projection under this fixture's winding, so the
        // clip snaps to the nearest edge segment (2,0)-(1,2), foot
        // (1.4,1.2) — squared distance 0.2 — and waypoint 0 still wins
        // over the degenerate segment waypoint (2.09 with its height
        // clamp). The engine-truth replay pins the wound-right sets.
        let point = Vector3::new(1.0, 0.7, 1.0);
        let mut best = f32::MAX;
        assert_eq!(set.find_closest(point, &mut best), 0, "the nearer wins");
        assert!((best - 0.2).abs() <= 1e-5, "the clipped distance, {best}");

        // A best already better than any waypoint rules all of them out.
        let mut best = 0.2;
        assert_eq!(set.find_closest(point, &mut best), -1, "nothing qualifies");
    }
}
