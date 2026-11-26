//! `lib/waypoint/chunk_waypoint.{hpp,cpp}` — the loaded waypoint polygon and
//! its edge adjacency encoding.
//!
//! A waypoint's edges live in the owning set's shared edge array
//! (`ChunkWaypointSetData::edges_`, handed out as `DependentArray` slices);
//! the port keeps the same shape with an offset/count pair over the set's
//! `Vec<Edge>`.

use super::{EdgeIndex, WaypointIndex};
use foundation::math::{Vector2, Vector3};
use std::cell::Cell;

/// `ChunkWaypoint::Edge::adjacentRangeStart` (`chunk_waypoint.cpp:49`):
/// neighbour values at or above this are not in-set waypoints.
pub const ADJACENT_RANGE_START: EdgeIndex = 32768;
/// `ChunkWaypoint::Edge::adjacentRangeEnd` (`chunk_waypoint.cpp:50`).
pub const ADJACENT_RANGE_END: EdgeIndex = 65535;
/// `ChunkWaypoint::Edge::otherChunk` (`chunk_waypoint.cpp:51`): the
/// neighbouring-waypoint answer for every out-of-range neighbour encoding.
pub const OTHER_CHUNK: WaypointIndex = -1;
/// `ChunkWaypoint::HEIGHT_RANGE_TOLERANCE` (`chunk_waypoint.cpp:56`).
pub const HEIGHT_RANGE_TOLERANCE: f32 = 0.1;

/// The vertex-provider interface (`chunk_waypoint_vertex_provider.hpp`):
/// the set data answers edge vertex indices with polygon coordinates.
pub trait ChunkWaypointVertexProvider {
    /// `vertexByIndex` — the reference `MF_ASSERT`s the index is in range.
    fn vertex_by_index(&self, index: EdgeIndex) -> &Vector2;
}

/// `ChunkWaypoint::Edge` (`chunk_waypoint.hpp:64-73`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Edge {
    /// `vertexIndex_`: the start coordinate of the edge.
    pub vertex_index: EdgeIndex,
    /// `neighbour_`: 0..32767 is the adjacent waypoint id, 32768..65535 is
    /// the chunk-boundary range, and top-bit-set values carry vista flags
    /// (cover) — "'adj' already encoded as we like it".
    pub neighbour: EdgeIndex,
}

impl Edge {
    /// `neighbouringWaypoint()` (`chunk_waypoint.hpp:88-92`): the adjacent
    /// waypoint's index, or `otherChunk` (-1) for every other encoding.
    pub fn neighbouring_waypoint(&self) -> WaypointIndex {
        if self.neighbour < ADJACENT_RANGE_START {
            self.neighbour as WaypointIndex
        } else {
            OTHER_CHUNK
        }
    }

    /// `adjacentToChunk()` (`chunk_waypoint.hpp:101-104`): the upper bound
    /// of the range check is commented out in the reference, so vista
    /// encodings report chunk-adjacent too.
    pub fn adjacent_to_chunk(&self) -> bool {
        self.neighbour >= ADJACENT_RANGE_START
    }
}

/// `closestPointOnLineSegment` (`chunk_waypoint.cpp:21-46`, anonymous
/// namespace): the projection when it lands within the segment, the nearer
/// endpoint otherwise.
fn closest_point_on_line_segment(start: Vector2, end: Vector2, point: &mut Vector2) {
    let dir = Vector2::new(end.x - start.x, end.y - start.y);
    let project = Vector2::new(point.x - start.x, point.y - start.y);
    let length = (dir.x * dir.x + dir.y * dir.y).sqrt();

    // dir.normalise()
    let norm = if length > 0.0 {
        Vector2::new(dir.x / length, dir.y / length)
    } else {
        dir
    };

    let dot = project.x * norm.x + project.y * norm.y;

    if (0.0..=length).contains(&dot) {
        *point = Vector2::new(start.x + norm.x * dot, start.y + norm.y * dot);
    } else if dot < 0.0 {
        *point = start;
    } else {
        *point = end;
    }
}

/// `ChunkWaypoint` (`chunk_waypoint.hpp:32-131`): one navpoly of the set.
#[derive(Debug, Default)]
pub struct ChunkWaypoint {
    /// `minHeight_`.
    pub min_height: f32,
    /// `maxHeight_`.
    pub max_height: f32,
    /// `centre_`: a point inside the waypoint ("it might not be the exact
    /// centre but could be used as an internal point in some cases").
    pub centre: Vector2,
    /// Start offset of this waypoint's run in the set's shared edge array
    /// (the `DependentArray` edges_ pointer).
    pub edge_start: u32,
    /// `edgeCount_`: "DO NOT move this away! DependentArray depends on
    /// edgeCount_ to exist on its construction."
    pub edge_count: u32,
    /// `visited_` — the search-marking flag the decode resets to zero.
    pub visited: Cell<EdgeIndex>,
}

impl ChunkWaypoint {
    /// `contains` (`chunk_waypoint.cpp:105-122`): the point is in the
    /// waypoint when its height sits within the min/max range (the
    /// `HEIGHT_RANGE_TOLERANCE` band, the reference spells the literal
    /// 0.1) **and** the projection test holds.
    pub fn contains(
        &self,
        edges: &[Edge],
        provider: &dyn ChunkWaypointVertexProvider,
        point: Vector3,
    ) -> bool {
        if point.y < self.min_height - 0.1 {
            return false;
        }
        if point.y > self.max_height + 0.1 {
            return false;
        }
        self.contains_projection(edges, provider, point)
    }

    /// `containsProjection` (`chunk_waypoint.cpp:137-172`): the x-z
    /// winding test — every edge must keep `c = xd*v - zd*u` above
    /// `-0.01` ("a fudge factor to accommodate floating point error").
    pub fn contains_projection(
        &self,
        edges: &[Edge],
        provider: &dyn ChunkWaypointVertexProvider,
        point: Vector3,
    ) -> bool {
        let edge_count = self.edge_count as usize;
        let run_start = self.edge_start as usize;

        let mut p_last = *provider.vertex_by_index(edges[run_start + edge_count - 1].vertex_index);

        for i in 0..edge_count {
            let this = *provider.vertex_by_index(edges[run_start + i].vertex_index);

            let u = this.x - p_last.x;
            let v = this.y - p_last.y;
            let xd = point.x - p_last.x;
            let zd = point.z - p_last.y;

            let c = xd * v - zd * u;
            if c <= -0.01 {
                return false;
            }

            p_last = this;
        }

        true
    }

    /// `makeMaxHeight` (`chunk_waypoint.cpp:311-317`): the point is moved
    /// to `maxHeight_`. The reference maps world↔waypoint space through
    /// the chunk; the port's waypoint-space model is the identity mapping
    /// (registered).
    pub fn make_max_height(&self, point: &mut Vector3) {
        point.y = self.max_height;
    }

    /// The waypoint's edge run in the set's shared edge array (the
    /// `DependentArray` begin/end pair).
    pub fn edge_range(&self) -> std::ops::Range<usize> {
        self.edge_start as usize..(self.edge_start + self.edge_count) as usize
    }

    /// `clip` (`chunk_waypoint.cpp:217-318`): the point is pulled into the
    /// polygon. A point outside the winding test snaps to the closest
    /// point on the closest edge segment; the height always lands on
    /// `maxHeight_` (the trailing `makeMaxHeight`).
    ///
    /// Registered boundary: the reference then validates the point against
    /// the chunk bounding box and, when it falls outside, nudges it in from
    /// the first non-chunk-adjacent edge midpoint; that needs the chunk
    /// face, and the fixture/identity mapping keeps every probe point
    /// inside it, so the port models the box as unbounded.
    pub fn clip(
        &self,
        edges: &[Edge],
        provider: &dyn ChunkWaypointVertexProvider,
        point: &mut Vector3,
    ) {
        let edge_count = self.edge_count as usize;
        let run_start = self.edge_start as usize;
        let back = *provider.vertex_by_index(edges[run_start + edge_count - 1].vertex_index);

        let mut outside_at = None;
        let mut prev = back;
        for i in 0..edge_count {
            let this = *provider.vertex_by_index(edges[run_start + i].vertex_index);

            let edge_vec = Vector2::new(this.x - prev.x, this.y - prev.y);
            let point_vec = Vector2::new(point.x - prev.x, point.z - prev.y);

            // `edgeVec.crossProduct( pointVec ) > 0.f` — the sign mirror of
            // containsProjection's `c > -0.01` inside test.
            if edge_vec.x * point_vec.y - edge_vec.y * point_vec.x > 0.0 {
                outside_at = Some(i);
                break;
            }

            prev = this;
        }

        if outside_at.is_some() {
            let p2d = Vector2::new(point.x, point.z);
            let mut p2d_best = p2d;
            let mut best_dist_squared = f32::MAX;
            let mut prev = back;

            for i in 0..edge_count {
                let now = *provider.vertex_by_index(edges[run_start + i].vertex_index);
                let mut temp = p2d;
                closest_point_on_line_segment(prev, now, &mut temp);

                let dist_squared =
                    (temp.x - p2d.x) * (temp.x - p2d.x) + (temp.y - p2d.y) * (temp.y - p2d.y);
                if dist_squared < best_dist_squared {
                    best_dist_squared = dist_squared;
                    p2d_best = temp;
                }

                prev = now;
            }

            point.x = p2d_best.x;
            point.z = p2d_best.y;
        }

        self.make_max_height(point);
    }

    /// `distanceSquared` (`chunk_waypoint.cpp:174-195`): the squared
    /// distance from the point to the waypoint — the point clipped into
    /// the polygon, with its height clamped into the min/max band instead
    /// of just `maxHeight_` (`clipPoint.y = Math::clamp( minHeight_,
    /// point.y, maxHeight_ )`; the clamp reads the original point).
    pub fn distance_squared(
        &self,
        edges: &[Edge],
        provider: &dyn ChunkWaypointVertexProvider,
        point: Vector3,
    ) -> f32 {
        let mut clip_point = point;
        self.clip(edges, provider, &mut clip_point);
        clip_point.y = point.y.max(self.min_height).min(self.max_height);
        let d = point - clip_point;
        d.x * d.x + d.y * d.y + d.z * d.z
    }

    /// `calcCentre` (`chunk_waypoint.cpp:365-387`): the length-weighted
    /// average of the edge midpoints,
    /// `centre += (end + start) / 2 * length; centre /= totalLength`.
    /// `edges` is the set's shared edge array; the waypoint walks its own
    /// run within it (`edges_[i].vertexIndex_`).
    pub fn calc_centre(&mut self, edges: &[Edge], provider: &dyn ChunkWaypointVertexProvider) {
        let mut total_length = 0.0f32;

        self.centre = Vector2::new(0.0, 0.0);

        let edge_count = self.edge_count as usize;
        let run_start = self.edge_start as usize;
        for i in 0..edge_count {
            let start = provider.vertex_by_index(edges[run_start + i].vertex_index);
            let end =
                provider.vertex_by_index(edges[run_start + (i + 1) % edge_count].vertex_index);

            let length = ((end.x - start.x) * (end.x - start.x)
                + (end.y - start.y) * (end.y - start.y))
                .sqrt();

            // centre_ += ( end + start ) / 2 * length
            self.centre.x += (end.x + start.x) / 2.0 * length;
            self.centre.y += (end.y + start.y) / 2.0 * length;
            total_length += length;
        }

        self.centre.x /= total_length;
        self.centre.y /= total_length;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Table(Vec<Vector2>);

    impl ChunkWaypointVertexProvider for Table {
        fn vertex_by_index(&self, index: EdgeIndex) -> &Vector2 {
            assert!((index as usize) < self.0.len(), "reference MF_ASSERT");
            &self.0[index as usize]
        }
    }

    fn square_provider() -> Table {
        // The engine's clockwise x-z winding (the truth fixture's cycle,
        // started at a different corner) — the winding tests accept it.
        Table(vec![
            Vector2::new(0.0, 0.0),
            Vector2::new(0.0, 2.0),
            Vector2::new(2.0, 2.0),
            Vector2::new(2.0, 0.0),
        ])
    }

    /// A waypoint over the whole provider edge range.
    fn square_waypoint(provider: &Table) -> ChunkWaypoint {
        ChunkWaypoint {
            min_height: 0.0,
            max_height: 1.0,
            centre: Vector2::default(),
            edge_start: 0,
            edge_count: provider.0.len() as u32,
            visited: Cell::new(0),
        }
    }

    #[test]
    fn edge_accessors_decode_adjacency_like_the_reference() {
        // In-range neighbour values are plain waypoint ids.
        let in_set = |n: EdgeIndex| {
            let edge = Edge {
                vertex_index: 0,
                neighbour: n,
            };
            (edge.neighbouring_waypoint(), edge.adjacent_to_chunk())
        };
        assert_eq!(in_set(0), (0, false));
        assert_eq!(in_set(1), (1, false));
        assert_eq!(in_set(32767), (32767, false));

        // Everything from adjacentRangeStart up answers otherChunk (-1) and
        // chunk-adjacent: the boundary encoding 65535 and the vista
        // encodings (top-bit cover flags, e.g. ~-5 = 0xFFFA) alike — the
        // reference's upper range check is commented out.
        assert_eq!(in_set(32768), (OTHER_CHUNK, true));
        assert_eq!(in_set(65530), (OTHER_CHUNK, true));
        assert_eq!(in_set(ADJACENT_RANGE_END), (OTHER_CHUNK, true));
        assert_eq!((ADJACENT_RANGE_START, ADJACENT_RANGE_END), (32768, 65535));
    }

    #[test]
    fn calc_centre_averages_length_weighted_edge_midpoints() {
        let provider = square_provider();
        let mut wp = square_waypoint(&provider);
        // Edges reference their vertices in run order.
        let edges: Vec<Edge> = (0..4)
            .map(|v| Edge {
                vertex_index: v,
                neighbour: 0,
            })
            .collect();
        wp.calc_centre(&edges, &provider);
        // The unit square's edge midpoints average to the geometric centre;
        // matches the fixture truth (gen_waypoints.log WP 0).
        assert!((wp.centre.x - 1.0).abs() <= 1e-6);
        assert!((wp.centre.y - 1.0).abs() <= 1e-6);

        // The fixture triangle B (2,0)-(4,0)-(2,2): the length-weighted
        // midpoint average pulls toward the long edges, centre
        // (2.707107, 0.707107) in the truth log. Its edges use the set-wide
        // vertex table (index 4 = (4,0)) and the run starts mid-array after
        // A's edges — proving the walk reads edge.vertexIndex_.
        let triangle = Table(vec![
            Vector2::new(0.0, 0.0),
            Vector2::new(2.0, 0.0),
            Vector2::new(2.0, 2.0),
            Vector2::new(0.0, 2.0),
            Vector2::new(4.0, 0.0),
            Vector2::new(4.0, 2.0),
        ]);
        let edges = [
            Edge {
                vertex_index: 2,
                neighbour: 0,
            }, // (2,2) — A's edge ahead of the run in the shared array
            Edge {
                vertex_index: 1,
                neighbour: 0,
            }, // (2,0)
            Edge {
                vertex_index: 4,
                neighbour: 0,
            }, // (4,0)
            Edge {
                vertex_index: 2,
                neighbour: 0,
            }, // (2,2)
        ];
        let mut wp = ChunkWaypoint {
            edge_start: 1,
            edge_count: 3,
            ..Default::default()
        };
        wp.calc_centre(&edges, &triangle);
        // The truth log prints (2.707107, 0.707107) = (2 + 1/sqrt(2),
        // 1/sqrt(2)) for the long-edge-weighted triangle centre.
        let expected_x = 2.0 + std::f32::consts::FRAC_1_SQRT_2;
        let expected_y = std::f32::consts::FRAC_1_SQRT_2;
        assert!((wp.centre.x - expected_x).abs() <= 1e-5);
        assert!((wp.centre.y - expected_y).abs() <= 1e-5);
    }

    #[test]
    fn the_edge_range_slices_the_shared_edge_array() {
        let provider = square_provider();
        let wp = square_waypoint(&provider);
        assert_eq!(wp.edge_range(), 0..4);
        assert_eq!(
            wp.edge_range().len(),
            wp.edge_count as usize,
            "edgeCount_ drives the DependentArray extent"
        );

        // A waypoint starting mid-array (the reference's pointer pair).
        let wp = ChunkWaypoint {
            edge_start: 7,
            edge_count: 3,
            ..Default::default()
        };
        assert_eq!(wp.edge_range(), 7..10);
    }

    #[test]
    fn the_vertex_provider_asserts_out_of_range_indices() {
        let provider = square_provider();
        assert_eq!(*provider.vertex_by_index(3), Vector2::new(2.0, 0.0));
        assert!(std::panic::catch_unwind(|| provider.vertex_by_index(4)).is_err());
    }

    #[test]
    fn the_distance_of_an_interior_point_in_the_band_is_zero() {
        let provider = square_provider();
        let wp = square_waypoint(&provider);
        let edges: Vec<Edge> = (0..4)
            .map(|v| Edge {
                vertex_index: v,
                neighbour: 0,
            })
            .collect();

        // Inside the projection, height inside 0..1: the clip is identity.
        assert_eq!(
            wp.distance_squared(&edges, &provider, Vector3::new(1.0, 0.7, 1.0)),
            0.0
        );
        // Still zero at the band's edges — the clamp keeps both ends.
        assert_eq!(
            wp.distance_squared(&edges, &provider, Vector3::new(1.9, 0.0, 0.1)),
            0.0
        );
        assert_eq!(
            wp.distance_squared(&edges, &provider, Vector3::new(1.9, 1.0, 0.1)),
            0.0
        );
    }

    #[test]
    fn the_distance_clamps_the_height_into_the_band() {
        let provider = square_provider();
        let wp = square_waypoint(&provider);
        let edges: Vec<Edge> = (0..4)
            .map(|v| Edge {
                vertex_index: v,
                neighbour: 0,
            })
            .collect();

        // Under the band: the x-z clip is identity, the y term is
        // (minHeight - y) squared — 0.5 below the floor.
        assert_eq!(
            wp.distance_squared(&edges, &provider, Vector3::new(1.0, -0.5, 1.0)),
            0.25
        );
        // Over the band: (y - maxHeight) squared — 2 above the roof.
        assert_eq!(
            wp.distance_squared(&edges, &provider, Vector3::new(1.0, 3.0, 1.0)),
            4.0
        );
    }

    #[test]
    fn the_distance_outside_the_polygon_measures_the_clip() {
        let provider = square_provider();
        let wp = square_waypoint(&provider);
        let edges: Vec<Edge> = (0..4)
            .map(|v| Edge {
                vertex_index: v,
                neighbour: 0,
            })
            .collect();

        // East of the square: the clip lands on the x = 2 edge, height in
        // band — (3 - 2) squared.
        assert_eq!(
            wp.distance_squared(&edges, &provider, Vector3::new(3.0, 0.5, 1.0)),
            1.0
        );
        // Diagonally outside: the clip lands on the corner (2, 0) —
        // (3 - 2) squared + (1 - 0) squared.
        assert_eq!(
            wp.distance_squared(&edges, &provider, Vector3::new(3.0, 0.5, -1.0)),
            2.0
        );
    }
}
