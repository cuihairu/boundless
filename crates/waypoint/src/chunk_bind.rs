//! `ChunkWaypointSet::bind()` (`chunk_waypoint_set.cpp:384-482`) — the
//! server-side wiring walk that turns a mounted set's another-chunk
//! edges into portal connections: for every neighbour edge the
//! constructor collected, try the three probe heights against the
//! chunk's portals (`Chunk::findBetterPortal`), project the midpoint
//! onto the far chunk and demand `ChunkNavigator::findExact` answer
//! there, then `connect()`.
//!
//! The walk needs the chunk faces (`chunk.cpp`): the mounted chunk's
//! portal list, the far chunks' world boxes and loaded flags, and each
//! far chunk's mounted sets (the `ChunkNavigator` `wpSets_`). Those
//! live in the chunk lane, so the port takes them as a fixture-shaped
//! `BindSpace` description and keeps the whole walk here.
//!
//! Registered boundaries (same answers, different bookkeeping):
//! - `MappedVector3` world/chunk conversions run through the chunk
//!   transform; the fixture chunks are identity-transformed, so chunk,
//!   geo and waypoint space share coordinates. The port does not model
//!   non-identity chunk transforms.
//! - `ChunkNavigator::findExact`'s girth-grid branch is out of scope:
//!   the grids only exist for outside chunks (`ChunkNavigator::add`
//!   bails before building them for inside chunks), and the port does
//!   not model the grid index. The linear `wpSets_` walk below is the
//!   face an inside chunk takes.
//! - `readyToBind`'s outside-chunk gate collapses the overlapper list
//!   into one flag (the fixture chunks are inside; the real gate wants
//!   every overlapping chunk loaded).

use super::chunk_waypoint::HEIGHT_RANGE_TOLERANCE;
use super::chunk_waypoint_set::Portal;
use super::chunk_waypoint_set_data::ChunkWaypointSetData;
use super::girth::almost_equal;
use super::{EdgeIndex, WaypointIndex};
use foundation::math::{BoundBox, PlaneEq, Vector3};
use std::cell::Cell;
use std::collections::BTreeMap;
use std::rc::Rc;

/// `NeighbourEdge` (`chunk_waypoint_set.hpp:36-46`): what the set
/// constructor records for one another-chunk edge — the three probe
/// heights and the edge midpoint in waypoint space (y unused, set per
/// attempt), plus `isBound` for the bind walk.
#[derive(Debug)]
pub struct NeighbourEdge {
    /// `yValues[3]`: the waypoint's mid-band (+0.1), its top band
    /// (+tolerance) and its bottom band (-tolerance).
    pub y_values: [f32; 3],
    /// `v`: the edge midpoint `(x, z)` in waypoint space.
    pub v: (f32, f32),
    /// `isBound`.
    pub is_bound: Cell<bool>,
}

/// `ChunkBoundary::Portal`'s geometry the bind walk reads, plus the
/// `readyToBind` bookkeeping. `points` are the rect corners in
/// `(uAxis, vAxis)` coordinates in the clockwise order
/// `findBetterPortal`'s inside test needs. `flags` is the `Portal`
/// face `connect()` stores (shared so tests can map a connection back
/// to its portal).
pub struct BindPortal {
    /// The portal plane (`Portal::plane`).
    pub plane: PlaneEq,
    /// `points` — the rect corners in the portal's (u, v) axes.
    pub points: Vec<(f32, f32)>,
    /// `uAxis`.
    pub u_axis: Vector3,
    /// `vAxis`.
    pub v_axis: Vector3,
    /// `origin` — the portal's centre in the mounted chunk's space;
    /// bind's projection lands the far point here plus the (u, v)
    /// dot products of the probe point.
    pub origin: Vector3,
    /// `hasChunk()` — the far chunk's `BindSpace` index. `None` is an
    /// unconnected portal (heaven/earth/extern).
    pub far: Option<usize>,
    /// The flags face `connect()` records.
    pub flags: Rc<Portal>,
    /// The portal sits in an `unboundPortals_` list (the ready gate's
    /// `ChunkBoundary::Portals` walk).
    pub unbound: bool,
    /// `hasChunkOrIsNothing()` = `!isConnectingToSpecial()`: a normal
    /// (non-heaven/earth/extern) portal.
    pub has_chunk_or_is_nothing: bool,
    /// `isInvasive()`.
    pub is_invasive: bool,
}

/// One mounted chunk as the bind walk sees it (`chunk.cpp` faces).
pub struct BindChunk {
    /// The world bounding box (`boundingBox()`): the portal scan's
    /// intersects gate and the smaller-chunk preference read it.
    pub bb: BoundBox,
    /// `isOutsideChunk()`.
    pub outside: bool,
    /// `loaded()`.
    pub loaded: bool,
    /// The outside-chunk overlapper gate collapsed: every overlapping
    /// chunk is loaded (`ChunkOverlappers::instance`'s walk).
    pub overlappers_loaded: bool,
    /// The portals of every joint, in `pbegin()..pend()` order.
    pub portals: Vec<BindPortal>,
    /// The mounted sets in `ChunkNavigator` `wpSets_` order.
    pub sets: Vec<Rc<ChunkWaypointSetData>>,
}

/// The chunk world one bind walk runs over.
pub struct BindSpace {
    /// The chunks, indexed by the `far` indices the portals carry.
    pub chunks: Vec<BindChunk>,
}

/// One collected neighbour edge as `neighbour_edges()` answers it:
/// `(absolute edge, (midpoint, yValues, isBound))`.
pub type NeighbourEdgeRecord = (EdgeIndex, (f32, f32), [f32; 3], bool);

impl ChunkWaypointSetData {
    /// The `ChunkWaypointSet` constructor's neighbour-edge collection
    /// (`chunk_waypoint_set.cpp:37-72`): every edge in the adjacent
    /// range (vista and world-boundary encodings included — they are
    /// `>= adjacentRangeStart` too) gets its `NeighbourEdge` keyed by
    /// the absolute edge index, off the waypoint's height band. The
    /// reference's `MF_ASSERT` refuses a duplicate absolute index.
    pub fn build_neighbour_edges(&self) {
        let mut edges = BTreeMap::new();
        for (waypoint_index, waypoint) in self.waypoints().iter().enumerate() {
            let wymin = waypoint.min_height;
            let wymax = waypoint.max_height;
            let wyavg = (wymin + wymax) * 0.5 + 0.1;
            let run = waypoint.edge_range();
            for local in 0..waypoint.edge_count as usize {
                let edge = &self.edges()[run.start + local];
                if !edge.adjacent_to_chunk() {
                    continue;
                }
                let absolute = self.absolute_edge_index(waypoint_index, local);
                assert!(
                    !edges.contains_key(&absolute),
                    "reference MF_ASSERT: duplicate absolute edge index"
                );
                let next_local = if local + 1 == waypoint.edge_count as usize {
                    0
                } else {
                    local + 1
                };
                let start = self.vertices()[edge.vertex_index as usize];
                let next = self.edges()[run.start + next_local];
                let end = self.vertices()[next.vertex_index as usize];
                edges.insert(
                    absolute,
                    NeighbourEdge {
                        y_values: [
                            wyavg,
                            wymax + HEIGHT_RANGE_TOLERANCE,
                            wymin - HEIGHT_RANGE_TOLERANCE,
                        ],
                        v: ((start.x + end.x) * 0.5, (start.y + end.y) * 0.5),
                        is_bound: Cell::new(false),
                    },
                );
            }
        }
        *self.neighbour_edges.borrow_mut() = edges;
    }

    /// The collected neighbour edges (the constructor's `neighbourEdges_`)
    /// in ascending edge order (the reference's `BW::map` over `EdgeIndex`).
    pub fn neighbour_edges(&self) -> Vec<NeighbourEdgeRecord> {
        self.neighbour_edges
            .borrow()
            .iter()
            .map(|(&edge, value)| (edge, value.v, value.y_values, value.is_bound.get()))
            .collect()
    }

    /// `ChunkWaypointSet::readyToBind`
    /// (`chunk_waypoint_set.cpp:369-409`): no unloaded overlapper (the
    /// collapsed flag, outside chunks only), no plain unbound portal in
    /// the joints (special heaven/earth/extern portals are the escape),
    /// and every far chunk of an own portal loaded.
    pub fn ready_to_bind(&self, space: &BindSpace, chunk_index: usize) -> bool {
        let chunk = &space.chunks[chunk_index];

        if chunk.outside && !chunk.overlappers_loaded {
            return false;
        }

        for portal in &chunk.portals {
            if portal.unbound && (portal.has_chunk_or_is_nothing || portal.is_invasive) {
                return false;
            }
        }

        for portal in &chunk.portals {
            if let Some(&far) = portal.far.as_ref()
                && !space.chunks[far].loaded
            {
                return false;
            }
        }

        true
    }

    /// `ChunkWaypointSet::bind()` (`chunk_waypoint_set.cpp:411-482`):
    /// walk the unbound neighbour edges, try the three probe heights
    /// against the chunk's portals, and connect through the first
    /// portal whose far side answers `findExact`.
    pub fn bind(self: &Rc<Self>, space: &BindSpace, chunk_index: usize) {
        if !self.ready_to_bind(space, chunk_index) {
            return;
        }

        let chunk = &space.chunks[chunk_index];
        let girth = self.girth();

        let keys: Vec<EdgeIndex> = self.neighbour_edges.borrow().keys().copied().collect();
        for edge_index in keys {
            let (y_values, midpoint) = {
                let edges = self.neighbour_edges.borrow();
                let edge = edges.get(&edge_index).expect("built edges");
                if edge.is_bound.get() {
                    continue;
                }
                (edge.y_values, edge.v)
            };

            for &y_probe in y_values.iter() {
                let wv = Vector3::new(midpoint.0, y_probe, midpoint.1);

                let mut chosen: Option<usize> = None;
                for (portal_index, portal) in chunk.portals.iter().enumerate() {
                    let Some(&far) = portal.far.as_ref() else {
                        continue;
                    };
                    let far_chunk = &space.chunks[far];
                    if !far_chunk.bb.contains(&wv) {
                        continue;
                    }
                    // only inside chunks test the distance from the
                    // portal plane
                    let min_dist = if far_chunk.outside { 0.0 } else { 1.0 };
                    if find_better_portal(space, chunk_index, chosen, min_dist, portal_index, wv) {
                        chosen = Some(portal_index);
                    }
                }

                let Some(cp) = chosen else {
                    continue;
                };

                let portal = &chunk.portals[cp];
                let far = portal.far.expect("the chosen portal's far chunk");

                // make sure we have a corresponding waypoint on the
                // other side of the portal
                let ltpv = portal.origin
                    + portal.u_axis * portal.u_axis.dot_product(&wv)
                    + portal.v_axis * portal.v_axis.dot_product(&wv);

                let Some((far_set, far_waypoint)) = find_exact(&space.chunks[far], ltpv, girth)
                else {
                    continue;
                };

                self.connect(
                    far_set,
                    Rc::clone(&portal.flags),
                    edge_index,
                    far_waypoint,
                    wv,
                );
                self.neighbour_edges.borrow()[&edge_index]
                    .is_bound
                    .set(true);

                break;
            }
        }
    }
}

/// `ChunkNavigator::findExact`'s linear face
/// (`chunk_navigator.cpp:279-363`, the no-grid fallback): the first
/// waypoint whose projection contains the point and whose band reaches
/// it answers; otherwise the best-height candidate (the closest
/// `maxHeight` below-or-at the probe) answers. The girth-grid branch is
/// a registered boundary (see the module header).
pub fn find_exact(
    chunk: &BindChunk,
    point: Vector3,
    girth: f32,
) -> Option<(Rc<ChunkWaypointSetData>, WaypointIndex)> {
    const INVALID_HEIGHT: f32 = 1_000_000.0;
    let mut best_height = INVALID_HEIGHT;
    let mut best: Option<(Rc<ChunkWaypointSetData>, WaypointIndex)> = None;

    for set in &chunk.sets {
        if !almost_equal(set.girth(), girth) {
            continue;
        }

        let edges = set.edges();
        for (i, waypoint) in set.waypoints().iter().enumerate() {
            if waypoint.min_height > point.y + HEIGHT_RANGE_TOLERANCE {
                continue;
            }

            if waypoint.contains_projection(edges, set.as_ref(), point) {
                if waypoint.max_height + HEIGHT_RANGE_TOLERANCE >= point.y {
                    return Some((Rc::clone(set), i as WaypointIndex));
                }

                // pick a relatively close navmesh with height range a
                // bit lower than the probe
                if (waypoint.max_height - point.y).abs() < (best_height - point.y).abs() {
                    best_height = waypoint.max_height;
                    best = Some((Rc::clone(set), i as WaypointIndex));
                }
            }
        }
    }

    if best_height != INVALID_HEIGHT {
        return best;
    }

    None
}

/// `Chunk::findBetterPortal` (`chunk.cpp:3576-3620`): does `test`
/// (portal `test_index` of chunk `chunk_index`) beat the incumbent
/// `curr` for the point `v`? The portal plane must sit within
/// `within_range` (inside chunks only), the point's (u, v) projection
/// must lie inside the rect, and competition resolves by the smaller
/// far chunk, then by the closer portal plane.
fn find_better_portal(
    space: &BindSpace,
    chunk_index: usize,
    curr: Option<usize>,
    within_range: f32,
    test_index: usize,
    v: Vector3,
) -> bool {
    let test = &space.chunks[chunk_index].portals[test_index];

    if within_range > 0.0 && test.plane.distance_to(&v).abs() > within_range {
        return false;
    }

    // projection of point onto portal plane must lie inside portal
    let mut inside = true;
    let pt2d = (test.u_axis.dot_product(&v), test.v_axis.dot_product(&v));
    let mut hpt = *test.points.last().expect("non-empty rect");
    for &tpt in &test.points {
        inside &= (tpt.0 - hpt.0) * (pt2d.1 - hpt.1) - (tpt.1 - hpt.1) * (pt2d.0 - hpt.0) > 0.0;
        hpt = tpt;
    }
    if !inside {
        return false;
    }

    // if there's no competition then test is the winner
    let Some(curr_index) = curr else {
        return true;
    };
    let curr_portal = &space.chunks[chunk_index].portals[curr_index];

    // prefer smaller chunks
    if test.far != curr_portal.far {
        let volume = |index: usize| -> f32 {
            let width = space.chunks[index].bb.width();
            width.x * width.y * width.z
        };
        return volume(test.far.expect("a competing portal's far chunk"))
            < volume(curr_portal.far.expect("the incumbent's far chunk"));
    }

    // prefer portals close to the test point
    test.plane.distance_to(&v).abs() < curr_portal.plane.distance_to(&v).abs()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One encoded set record: one square waypoint over the given
    /// edges, on the given height band and girth.
    fn band_set_girth(
        band: (f32, f32),
        edges: &[(f32, f32, i32)],
        girth: f32,
    ) -> Rc<ChunkWaypointSetData> {
        let mut b = Vec::new();
        let put_f = |b: &mut Vec<u8>, v: f32| b.extend_from_slice(&v.to_le_bytes());
        let put_i = |b: &mut Vec<u8>, v: i32| b.extend_from_slice(&v.to_le_bytes());
        put_i(&mut b, 0);
        put_f(&mut b, girth);
        put_i(&mut b, 1);
        put_i(&mut b, edges.len() as i32);
        put_f(&mut b, band.0);
        put_f(&mut b, band.1);
        put_i(&mut b, edges.len() as i32);
        for (x, y, n) in edges {
            put_f(&mut b, *x);
            put_f(&mut b, *y);
            put_i(&mut b, *n);
        }
        let mut sets =
            super::super::chunk_waypoint_set_data::parse_world_navmesh(&b).expect("decode");
        Rc::new(sets.remove(0))
    }

    fn band_set(band: (f32, f32), edges: &[(f32, f32, i32)]) -> Rc<ChunkWaypointSetData> {
        band_set_girth(band, edges, 0.6)
    }

    /// A ground band [4, 4] square x in [4.2, 5.1] z in [4, 5],
    /// clockwise in the engine's x-z sense (west +z, north +x, east -z,
    /// south -x): its east edge (x = 5.1 — the reference's ±1m overlap
    /// band, deliberately off the chunk-box boundary) carries the seam
    /// encoding 65535, the other three the vista form 65530 (also in the
    /// adjacent range — all four enter the neighbour-edge collection,
    /// like the navgen map's boundary/vista edges).
    fn seam_set() -> Rc<ChunkWaypointSetData> {
        band_set(
            (4.0, 4.0),
            &[
                (4.2, 4.0, 65530),
                (4.2, 5.0, 65530),
                (5.1, 5.0, 65535),
                (5.1, 4.0, 65530),
            ],
        )
    }

    fn flags(permissive: bool) -> Rc<Portal> {
        Rc::new(Portal {
            permissive,
            has_chunk_item: false,
        })
    }

    fn portal_on_seam(far: usize, flags: &Rc<Portal>) -> BindPortal {
        BindPortal {
            plane: PlaneEq::new(Vector3::new(1.0, 0.0, 0.0), 5.0),
            points: vec![(4.4, 5.4), (4.4, 3.6), (5.6, 3.6), (5.6, 5.4)],
            u_axis: Vector3::new(1.0, 0.0, 0.0),
            v_axis: Vector3::new(0.0, 0.0, 1.0),
            origin: Vector3::new(0.0, 4.1, 0.0),
            far: Some(far),
            flags: Rc::clone(flags),
            unbound: false,
            has_chunk_or_is_nothing: false,
            is_invasive: false,
        }
    }

    /// B's ground band [4, 4] square x in [5, 6] z in [4, 5], the same
    /// clockwise winding — its west edge sits at the portal plane, so
    /// a probe at x 5.1 lands strictly inside.
    fn far_set() -> Rc<ChunkWaypointSetData> {
        band_set(
            (4.0, 4.0),
            &[
                (5.0, 4.0, 65530),
                (5.0, 5.0, 65530),
                (6.0, 5.0, 65535),
                (6.0, 4.0, 65530),
            ],
        )
    }

    fn two_chunk_space(a: &Rc<ChunkWaypointSetData>, b: &Rc<ChunkWaypointSetData>) -> BindSpace {
        BindSpace {
            chunks: vec![
                BindChunk {
                    bb: BoundBox::new(Vector3::new(0.0, 0.0, 0.0), Vector3::new(5.0, 15.0, 10.0)),
                    outside: false,
                    loaded: true,
                    overlappers_loaded: false,
                    portals: vec![portal_on_seam(1, &flags(true))],
                    sets: vec![Rc::clone(a)],
                },
                BindChunk {
                    bb: BoundBox::new(Vector3::new(5.0, 0.0, 0.0), Vector3::new(10.0, 15.0, 10.0)),
                    outside: false,
                    loaded: true,
                    overlappers_loaded: false,
                    portals: vec![portal_on_seam(0, &flags(true))],
                    sets: vec![Rc::clone(b)],
                },
            ],
        }
    }

    fn near_tuple(a: (f32, f32), b: (f32, f32)) -> bool {
        (a.0 - b.0).abs() < 1e-5 && (a.1 - b.1).abs() < 1e-5
    }

    #[test]
    fn the_neighbour_edge_collection_keys_and_heights_match_the_ctor() {
        let set = seam_set();
        set.build_neighbour_edges();

        // All four edges are in the adjacent range (vista 65530
        // included); keys are the absolute indices (one waypoint, so
        // local == absolute).
        let collected = set.neighbour_edges();
        let keys: Vec<EdgeIndex> = collected.iter().map(|&(edge, ..)| edge).collect();
        assert_eq!(keys, [0, 1, 2, 3]);

        // yValues from the band [4, 4]: mid 4.1, top 4.1, bottom 3.9.
        // Edge midpoints: e0 (4.2, 4.5), e1 (4.65, 5), e2 (5.1, 4.5),
        // e3 (4.65, 4).
        let mids: Vec<(f32, f32)> = collected.iter().map(|&(_, mid, ..)| mid).collect();
        assert!(near_tuple(mids[0], (4.2, 4.5)), "{mids:?}");
        assert!(near_tuple(mids[1], (4.65, 5.0)), "{mids:?}");
        assert!(near_tuple(mids[2], (5.1, 4.5)), "{mids:?}");
        assert!(near_tuple(mids[3], (4.65, 4.0)), "{mids:?}");
        for &(_, _, y_values, bound) in collected.iter() {
            assert_eq!(y_values, [4.1, 4.1, 3.9]);
            assert!(!bound);
        }
    }

    #[test]
    fn bind_wires_the_seam_edge_through_the_permissive_portal() {
        let a = seam_set();
        let b = far_set();
        let mut space = two_chunk_space(&a, &b);
        // An unconnected portal (heaven/earth/extern shape) ahead of the
        // real one: the scan's `hasChunk()` arm skips it.
        space.chunks[0].portals.insert(
            0,
            BindPortal {
                far: None,
                ..portal_on_seam(1, &flags(true))
            },
        );
        a.build_neighbour_edges();

        a.bind(&space, 0);
        // A second bind walks the already-bound edge and skips it.
        a.bind(&space, 0);

        // Only the east edge (2, midpoint (5.1, 4.5)) clears the far-bb
        // gate: its first probe height 4.1 passes the rect, and the
        // projection (5.1, 4.1, 4.5) lands strictly inside B's square
        // band [4, 4].
        let label = a.edge_label(2).expect("edge 2 wired");
        assert!(Rc::ptr_eq(&label.set, &b));
        assert_eq!(label.waypoint, 0);
        assert!(near_tuple(
            (label.entry_point.x, label.entry_point.z),
            (5.1, 4.5)
        ));
        assert!((label.entry_point.y - 4.1).abs() < 1e-5, "mid-band probe");

        // The other three edges never find a portal: their midpoints
        // sit west of the seam, outside B's box.
        assert!(a.edge_label(0).is_none());
        assert!(a.edge_label(1).is_none());
        assert!(a.edge_label(3).is_none());

        let collected = a.neighbour_edges();
        assert!(collected[2].3, "edge 2 marked bound");
        assert!(!collected[0].3 && !collected[1].3 && !collected[3].3);

        // The connection landed through A's portal flags (still index 1
        // behind the dead portal; and exactly once after the re-bind).
        let conns = a.connections();
        assert_eq!(conns.len(), 1);
        assert!(Rc::ptr_eq(&conns[0].0, &b));
        assert!(Rc::ptr_eq(&conns[0].1, &space.chunks[0].portals[1].flags));
    }

    #[test]
    fn a_probe_the_far_side_refuses_leaves_the_edge_unbound() {
        // B's west band square spans z [4, 5]; a seam edge whose midpoint
        // sits at z 3.7 clears the far box and the portal rect (v
        // [3.6, 5.4]) but its projection lands on no waypoint — the
        // reference's "no corresponding waypoint on the other side"
        // refusal, tried at every probe height.
        let a = band_set(
            (4.0, 4.0),
            &[
                (4.2, 3.4, 65530),
                (4.2, 4.0, 65530),
                (5.1, 4.0, 65535),
                (5.1, 3.4, 65530),
            ],
        );
        let b = far_set();
        let space = two_chunk_space(&a, &b);
        a.build_neighbour_edges();
        a.bind(&space, 0);

        assert!(a.edge_label(0).is_none(), "edge 0 (west) never crosses");
        assert!(a.edge_label(1).is_none());
        assert!(
            a.edge_label(2).is_none(),
            "the seam edge's projection misses B"
        );
        assert!(a.edge_label(3).is_none());
        assert!(a.connections().is_empty(), "nothing connects");
    }

    #[test]
    fn a_shut_ready_gate_blocks_the_whole_walk() {
        let a = seam_set();
        let b = far_set();
        let mut space = two_chunk_space(&a, &b);
        a.build_neighbour_edges();

        // An unloaded far chunk (the own-portals gate).
        space.chunks[1].loaded = false;
        a.bind(&space, 0);
        assert!(a.edge_label(2).is_none(), "nothing wired while B unloads");
        space.chunks[1].loaded = true;

        // A plain unbound portal in the joints (the unbound-portals
        // gate); the special/invasive escapes are the recorded arms.
        space.chunks[0].portals.push(BindPortal {
            unbound: true,
            has_chunk_or_is_nothing: true,
            is_invasive: false,
            ..portal_on_seam(1, &flags(true))
        });
        a.bind(&space, 0);
        assert!(a.edge_label(2).is_none(), "the gate stays shut");
        space.chunks[0].portals.pop();

        // The outside-chunk overlapper gate.
        space.chunks[0].outside = true;
        space.chunks[0].overlappers_loaded = false;
        a.bind(&space, 0);
        assert!(a.edge_label(2).is_none(), "unloaded overlappers block");
        space.chunks[0].overlappers_loaded = true;
        a.bind(&space, 0);
        assert!(a.edge_label(2).is_some(), "ready now");
    }

    #[test]
    fn the_best_height_fallback_takes_the_closest_max_height() {
        // Two overlapping sets on one chunk, both containing the probe
        // in projection, both bands short of it: the closest maxHeight
        // (the lower set, reached second) wins on the strict <.
        let square = [
            (5.0, 4.0, -1),
            (5.0, 5.0, -1),
            (6.0, 5.0, -1),
            (6.0, 4.0, -1),
        ];
        let high = band_set((2.0, 2.0), &square);
        let low = band_set((4.0, 4.0), &square);
        let probe = BindChunk {
            bb: BoundBox::new(Vector3::new(0.0, 0.0, 0.0), Vector3::new(10.0, 15.0, 10.0)),
            outside: false,
            loaded: true,
            overlappers_loaded: false,
            portals: Vec::new(),
            sets: vec![Rc::clone(&high), Rc::clone(&low)],
        };
        let found = find_exact(&probe, Vector3::new(5.5, 6.0, 4.5), 0.6).expect("best height");
        assert!(Rc::ptr_eq(&found.0, &low), "the closest band wins");
        assert_eq!(found.1, 0);

        // A probe whose band reaches it answers exactly on the first
        // containing set.
        let found = find_exact(&probe, Vector3::new(5.5, 2.05, 4.5), 0.6).expect("exact");
        assert!(Rc::ptr_eq(&found.0, &high));
        assert_eq!(found.1, 0);

        // A probe below every minHeight is refused outright.
        assert!(find_exact(&probe, Vector3::new(5.5, 1.0, 4.5), 0.6).is_none());

        // A girth mismatch refuses the whole set.
        let mismatched = band_set_girth(
            (2.0, 2.0),
            &[
                (5.0, 4.0, -1),
                (5.0, 5.0, -1),
                (6.0, 5.0, -1),
                (6.0, 4.0, -1),
            ],
            1.2,
        );
        let girth_probe = BindChunk {
            bb: BoundBox::new(Vector3::new(0.0, 0.0, 0.0), Vector3::new(10.0, 15.0, 10.0)),
            outside: false,
            loaded: true,
            overlappers_loaded: false,
            portals: Vec::new(),
            sets: vec![mismatched],
        };
        let found = find_exact(&girth_probe, Vector3::new(5.5, 2.05, 4.5), 0.6);
        assert!(found.is_none(), "the only set wears another girth");
    }

    #[test]
    fn the_portal_competition_arms() {
        let a = seam_set();
        let b = far_set();
        let mut space = two_chunk_space(&a, &b);

        // Shrink chunk B so the volume arm has something to chew: a
        // second portal still pointing at B loses nothing, but flip
        // the incumbent's far to A and the volume arm answers.
        space.chunks[0].bb =
            BoundBox::new(Vector3::new(0.0, 0.0, 0.0), Vector3::new(5.0, 7.0, 10.0));
        let probe = Vector3::new(5.1, 4.1, 4.5);

        // No competition: the test portal wins outright.
        assert!(find_better_portal(&space, 0, None, 1.0, 0, probe));

        // Same far chunk (B), strict-closer plane: at x 5.1 the planes
        // d=5 and d=5.2 tie in |distance| — the incumbent keeps.
        space.chunks[0]
            .portals
            .push(portal_on_seam(1, &flags(true)));
        space.chunks[0].portals[1].plane = PlaneEq::new(Vector3::new(1.0, 0.0, 0.0), 5.2);
        assert!(!find_better_portal(&space, 0, Some(0), 1.0, 1, probe));
        // Closer to the second plane: it wins.
        let probe2 = Vector3::new(5.15, 4.1, 4.5);
        assert!(find_better_portal(&space, 0, Some(0), 1.0, 1, probe2));

        // Different far chunks: incumbent portal 0 (far B, volume
        // 750), test portal 2 (far A, volume 350) — the smaller far
        // chunk wins regardless of distance.
        let mut to_a = portal_on_seam(0, &flags(true));
        to_a.plane = PlaneEq::new(Vector3::new(1.0, 0.0, 0.0), 5.1);
        space.chunks[0].portals.push(to_a);
        assert!(find_better_portal(&space, 0, Some(0), 1.0, 2, probe));
    }

    #[test]
    fn the_inside_and_range_gates_refuse_out_of_rect_probes() {
        let a = seam_set();
        let b = far_set();
        let space = two_chunk_space(&a, &b);

        // Outside the rect in u (west of the rect's 4.4).
        let outside_u = Vector3::new(4.0, 4.1, 4.5);
        assert!(!find_better_portal(&space, 0, None, 1.0, 0, outside_u));
        // Outside the rect in v (z beyond the rect's 5.4).
        let outside_v = Vector3::new(5.0, 4.1, 6.0);
        assert!(!find_better_portal(&space, 0, None, 1.0, 0, outside_v));
        // Beyond the plane-distance gate (inside chunk: 1 m).
        let far_off = Vector3::new(6.5, 4.1, 4.5);
        assert!(!find_better_portal(&space, 0, None, 1.0, 0, far_off));
        // No distance gate at all when the far chunk is outside.
        let mut outside_space = two_chunk_space(&a, &b);
        outside_space.chunks[1].outside = true;
        assert!(find_better_portal(
            &outside_space,
            0,
            None,
            0.0,
            0,
            Vector3::new(5.5, 4.1, 4.5)
        ));
    }
}
