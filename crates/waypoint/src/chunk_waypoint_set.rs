//! `lib/waypoint/chunk_waypoint_set.{hpp,cpp}` — the runtime connection
//! face over a decoded waypoint set: the portal-brokered connections to
//! neighbouring chunks' sets, the per-edge labels they were made through,
//! the backlinks the neighbours keep, and the boundary-edge records the
//! constructor fills for `bind()`.
//!
//! The reference wraps `ChunkWaypointSetData` in a `ChunkWaypointSet`
//! (chunk-mounted, refcounted) holding `connections_`/`edgeLabels_`/
//! `backlinks_`/`neighbourEdges_`; the port keeps those as `RefCell`
//! fields on the data record itself (same crate, so the inherent impls
//! below are legal). `bind()`/`readyToBind()`/`toss()` walk the chunk
//! face (`pChunk_->joints()`, the portal projection, the per-chunk
//! `ChunkNavigator`) and stay in the chunk lane; `connect()` is the
//! wiring face their output drives.
//!
//! Registered boundaries (same answers, different bookkeeping):
//! - `connections_` is a `BW::map<ChunkWaypointSetPtr, Portal*>` ordered
//!   by the set pointer; the port keys a `BTreeMap` by the `Rc` address —
//!   the same pointer ordering modulo the allocator's placement, which is
//!   not part of the contract.
//! - `edgeLabels_` is a `BW::unordered_map`; the port's `BTreeMap`
//!   iterates ascending edge index. Its one order-sensitive consumer
//!   (the set-level nearest-entry scan, a strict `<`) only differs on
//!   exact distance ties.
//! - `Portal` keeps the two flags and the destination chunk identity the
//!   waypoint faces read (`permissive`, `hasChunkItem()`, `pChunk`); the
//!   geometry stays in the chunk lane.
//! - `connectionPortal` is the reference's `connections_[pWaypointSet]` —
//!   a `BW::map::operator[]` that *inserts* a null portal for a missing
//!   key; the port answers `None` and inserts nothing.

use super::chunk_waypoint::{ChunkWaypointVertexProvider, HEIGHT_RANGE_TOLERANCE, OTHER_CHUNK};
use super::chunk_waypoint_set_data::ChunkWaypointSetData;
use super::{ADJACENT_RANGE_END, EdgeIndex, WaypointIndex};
use foundation::math::Vector3;
use std::collections::BTreeMap;
use std::rc::Rc;

/// `ChunkWaypointConns` (`chunk_waypoint_set.hpp:25-26`): the
/// neighbouring set and the portal the connection was made through,
/// keyed by the set address (the reference's pointer-ordered `BW::map`).
pub(crate) type ChunkWaypointConns = BTreeMap<usize, (Rc<ChunkWaypointSetData>, Rc<Portal>)>;

/// The two portal facts the waypoint faces read
/// (`chunk_boundary.hpp`): whether the portal may be crossed freely
/// (`permissive`), whether a chunk item has claimed it (`hasChunkItem`),
/// and the chunk the portal leads to (`pChunk`, read by the set-level
/// shell formula).
#[derive(Debug, Clone, Default)]
pub struct Portal {
    /// `permissive`.
    pub permissive: bool,
    /// `hasChunkItem_`.
    pub has_chunk_item: bool,
    /// `pChunk` as a chunk identity; `None` is the dangling portal.
    pub to_chunk: Option<usize>,
}

/// `EdgeLabel` (`chunk_waypoint_set.hpp:27-32`): what `connect` records
/// for one boundary edge — the neighbouring set, the waypoint it landed
/// on and the world-space point the connection was made through.
#[derive(Debug, Clone)]
pub struct EdgeLabel {
    /// `pSet_`.
    pub set: Rc<ChunkWaypointSetData>,
    /// `waypoint_` (`-1` never survives a real `bind()`).
    pub waypoint: WaypointIndex,
    /// `entryPoint_`.
    pub entry_point: Vector3,
}

/// `NeighbourEdge` (`chunk_waypoint_set.hpp:39-44`): one chunk-adjacent
/// edge, with the three bind probe heights and the edge midpoint `bind()`
/// walks from.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NeighbourEdge {
    /// `yValues`: the band centre, `maxHeight + HEIGHT_RANGE_TOLERANCE`
    /// and `minHeight - HEIGHT_RANGE_TOLERANCE`.
    pub y_values: [f32; 3],
    /// `v`: the edge midpoint (`(start + end) / 2`), height zero — the
    /// bind probe raises it per height.
    pub v: Vector3,
    /// `isBound`: set by `bind()` once the edge found its portal, cleared
    /// by `deleteConnection`/`removeOurConnections`.
    pub is_bound: bool,
}

impl ChunkWaypointSetData {
    /// The `ChunkWaypointSet` constructor's face
    /// (`chunk_waypoint_set.cpp:33-107`): one `NeighbourEdge` per
    /// chunk-adjacent edge, keyed by the absolute edge index, with the
    /// three bind probe heights off the owning waypoint's band and `v`
    /// the midpoint between the edge's vertex and the next edge's
    /// (wrapping). The reference `MF_ASSERT`s the index is fresh.
    pub fn build_neighbour_edges(&self) {
        let mut edges = self.neighbour_edges.borrow_mut();
        for waypoint in 0..self.waypoints().len() {
            let wp = &self.waypoints()[waypoint];
            let wymin = wp.min_height;
            let wymax = wp.max_height;
            let wyavg = (wymin + wymax) * 0.5 + 0.1;
            let range = wp.edge_range();
            for slot in range.clone() {
                let cwe = &self.edges()[slot];
                if !cwe.adjacent_to_chunk() {
                    continue;
                }
                let absolute = self.absolute_edge_index(waypoint, slot - range.start);
                assert!(
                    !edges.contains_key(&absolute),
                    "reference MF_ASSERTs the absolute edge index is fresh"
                );

                // The next edge in the polygon, wrapping past the end
                // (`nextEdgeIter`, chunk_waypoint_set.cpp:79-86).
                let next_slot = if slot + 1 == range.end {
                    range.start
                } else {
                    slot + 1
                };
                let start = *self.vertex_by_index(cwe.vertex_index);
                let end = *self.vertex_by_index(self.edges()[next_slot].vertex_index);

                edges.insert(
                    absolute,
                    NeighbourEdge {
                        y_values: [
                            wyavg,
                            wymax + HEIGHT_RANGE_TOLERANCE,
                            wymin - HEIGHT_RANGE_TOLERANCE,
                        ],
                        v: Vector3::new((start.x + end.x) / 2.0, 0.0, (start.y + end.y) / 2.0),
                        is_bound: false,
                    },
                );
            }
        }
    }

    /// The neighbour edges in ascending absolute-edge-index order.
    pub fn neighbour_edges(&self) -> Vec<(EdgeIndex, NeighbourEdge)> {
        self.neighbour_edges
            .borrow()
            .iter()
            .map(|(edge, entry)| (*edge, *entry))
            .collect()
    }

    /// `ChunkWaypointSet::connect` (`chunk_waypoint_set.cpp:293-324`):
    /// record a neighbouring set reached through `portal` over absolute
    /// edge `edge_index`, landing on `waypoint` at `entry_point`. A set's
    /// first connection is inserted and the neighbour backlinks us
    /// (`pWaypointSet->addBacklink( this )`); the same set through a
    /// different portal is the NOTICE and leaves the stored portal alone;
    /// the edge label is overwritten unconditionally. No geometry or
    /// adjacency check — the callers own that.
    pub fn connect(
        self: &Rc<Self>,
        set: Rc<ChunkWaypointSetData>,
        waypoint: WaypointIndex,
        portal: Rc<Portal>,
        edge_index: EdgeIndex,
        entry_point: Vector3,
    ) {
        let key = Rc::as_ptr(&set) as usize;
        {
            let mut connections = self.connections.borrow_mut();
            match connections.get(&key) {
                None => {
                    connections.insert(key, (Rc::clone(&set), Rc::clone(&portal)));
                    set.add_backlink(Rc::clone(self));
                }
                Some((_, stored)) => {
                    if !Rc::ptr_eq(stored, &portal) {
                        // NOTICE_MSG in the reference: "Chunk %s is
                        // connected to chunk %s via two different
                        // portals."
                        eprintln!(
                            "NOTICE_MSG: ChunkWaypointSet::connect: existing connection through a different portal"
                        );
                    }
                }
            }
        }
        self.edge_labels.borrow_mut().insert(
            edge_index,
            EdgeLabel {
                set,
                waypoint,
                entry_point,
            },
        );
    }

    /// `connectionPortal` (`chunk_waypoint_set.hpp:185-188`): the portal
    /// this set's connection to `set` was made through.
    pub fn connection_portal(&self, set: &Rc<ChunkWaypointSetData>) -> Option<Rc<Portal>> {
        self.connections
            .borrow()
            .get(&(Rc::as_ptr(set) as usize))
            .map(|(_, portal)| Rc::clone(portal))
    }

    /// `connectionWaypoint` (`chunk_waypoint_set.hpp:197-208`): the
    /// neighbouring set absolute edge `edge_index` was labelled with.
    pub fn connection_waypoint(&self, edge_index: EdgeIndex) -> Option<Rc<ChunkWaypointSetData>> {
        self.edge_labels
            .borrow()
            .get(&edge_index)
            .map(|label| Rc::clone(&label.set))
    }

    /// `edgeLabel` (`chunk_waypoint_set.hpp:218-232`): the label recorded
    /// for an edge. The reference answers a function-static
    /// `{NULL, -1, Vector3()}` record on a miss; the port answers `None`
    /// (the null `pSet_` its callers test for).
    pub fn edge_label(&self, edge_index: EdgeIndex) -> Option<EdgeLabel> {
        self.edge_labels.borrow().get(&edge_index).cloned()
    }

    /// The connections in set-address order (`connectionsBegin`/`End`).
    pub fn connections(&self) -> Vec<(Rc<ChunkWaypointSetData>, Rc<Portal>)> {
        self.connections
            .borrow()
            .values()
            .cloned()
            .collect::<Vec<_>>()
    }

    /// The edge labels in ascending edge-index order
    /// (`edgeLabelsBegin`/`End`).
    pub fn edge_labels(&self) -> Vec<(EdgeIndex, EdgeLabel)> {
        self.edge_labels
            .borrow()
            .iter()
            .map(|(edge, label)| (*edge, label.clone()))
            .collect()
    }

    /// `ChunkWaypointSet::removeOurConnections`
    /// (`chunk_waypoint_set.cpp:120-149`): clear our labels, drop us from
    /// every neighbour's backlinks, clear the connections, mark every
    /// neighbour edge unbound.
    pub fn remove_our_connections(self: &Rc<Self>) {
        self.edge_labels.borrow_mut().clear();

        for (set, _) in self.connections() {
            set.remove_backlink(self);
        }

        self.connections.borrow_mut().clear();

        for edge in self.neighbour_edges.borrow_mut().values_mut() {
            edge.is_bound = false;
        }
    }

    /// `ChunkWaypointSet::deleteConnection`
    /// (`chunk_waypoint_set.cpp:151-195`): remove the connection to
    /// `p_set` — our labels for it (each neighbour edge unbound), our
    /// entry in its backlinks, the connection itself. A missing
    /// connection is the reference's ERROR_MSG and a no-op.
    pub fn delete_connection(self: &Rc<Self>, p_set: &Rc<ChunkWaypointSetData>) {
        let key = Rc::as_ptr(p_set) as usize;
        if !self.connections.borrow().contains_key(&key) {
            // ERROR_MSG "connection from %u/%s to %u/%s does not exist".
            eprintln!("ERROR_MSG: ChunkWaypointSet::deleteConnection: connection does not exist");
            return;
        }

        // (1) remove our edge labels for this chunk waypoint set, and
        // mark their neighbour edges unbound.
        let removing: Vec<EdgeIndex> = self
            .edge_labels
            .borrow()
            .iter()
            .filter(|(_, label)| Rc::ptr_eq(&label.set, p_set))
            .map(|(edge, _)| *edge)
            .collect();
        for edge in removing {
            self.edge_labels.borrow_mut().remove(&edge);
            if let Some(entry) = self.neighbour_edges.borrow_mut().get_mut(&edge) {
                entry.is_bound = false;
            }
        }

        // (2) now remove from the backlinks list of the set we connected
        // to.
        p_set.remove_backlink(self);

        // (3) now remove from us, and all trace of connection is gone!
        self.connections.borrow_mut().remove(&key);
    }

    /// `ChunkWaypointSet::removeOthersConnections`
    /// (`chunk_waypoint_set.cpp:198-225`): walk the backlinks and have
    /// each linker delete its connection to us. A backlink without a
    /// matching connection is the reference's ERROR_MSG; the entry is
    /// dropped and the walk goes on (that entry is what
    /// `deleteConnection` cannot remove).
    pub fn remove_others_connections(self: &Rc<Self>) {
        while !self.backlinks.borrow().is_empty() {
            let p_set = self.backlinks.borrow().last().unwrap().clone();

            // loop around the connections in this waypointSet with a
            // backlink to us looking for it.
            let key = Rc::as_ptr(self) as usize;
            if !p_set.connections.borrow().contains_key(&key) {
                // ERROR_MSG "Back connection not found."
                eprintln!(
                    "ERROR_MSG: ChunkWaypointSet::removeOthersConnections: back connection not found"
                );
                self.backlinks.borrow_mut().pop();
                continue;
            }

            // will delete the current backlink from our list.
            p_set.delete_connection(&Rc::clone(self));
        }
    }

    /// `ChunkWaypointSet::addBacklink` (`chunk_waypoint_set.cpp:228-231`):
    /// a plain append — duplicates are the caller's problem, as in the
    /// reference.
    pub fn add_backlink(&self, set: Rc<ChunkWaypointSetData>) {
        self.backlinks.borrow_mut().push(set);
    }

    /// `ChunkWaypointSet::removeBacklink`
    /// (`chunk_waypoint_set.cpp:239-252`): erase the first occurrence; a
    /// missing entry is the reference's ERROR_MSG and a no-op.
    pub fn remove_backlink(&self, set: &Rc<ChunkWaypointSetData>) {
        let mut backlinks = self.backlinks.borrow_mut();
        if let Some(position) = backlinks.iter().position(|other| Rc::ptr_eq(other, set)) {
            backlinks.remove(position);
        } else {
            // ERROR_MSG "trying to remove backlink that doesn't exist".
            eprintln!("ERROR_MSG: ChunkWaypointSet::removeBacklink: no such backlink");
        }
    }

    /// The backlink list (`backlinks_`), cloned out.
    pub fn backlinks(&self) -> Vec<Rc<ChunkWaypointSetData>> {
        self.backlinks.borrow().clone()
    }

    /// Mount the set on a chunk identity: the shell formula and the
    /// chunk-gone guards consume `chunk()` on the reference's mounted
    /// set. Ids are opaque (the replay harness allots one per chunk);
    /// unmounted is the reference's missing `chunk()`.
    pub fn set_chunk_identity(&self, id: usize) {
        self.chunk_id.set(Some(id));
    }

    /// The mounted chunk identity (`chunk()`), or `None` while unmounted.
    pub fn chunk_identity(&self) -> Option<usize> {
        self.chunk_id.get()
    }

    /// `Chunk::isOutsideChunk` (`chunk.cpp:532-535`) as seen by the shell
    /// formula: the mounted chunk's outside flag. Defaults inside.
    pub fn set_outside_chunk(&self, outside: bool) {
        self.outside_chunk.set(outside);
    }

    /// The outside flag of the mounted chunk.
    pub fn is_outside_chunk(&self) -> bool {
        self.outside_chunk.get()
    }
}

/// `WaypointNeighbourIterator`
/// (`waypoint_neighbour_iterator.{hpp,cpp}`): walks a waypoint's
/// neighbourhood, crossing into neighbouring chunks through permissive
/// portals only.
///
/// The reference's constructor seeds `currentEdgeIndex_` with
/// `adjacentRangeEnd` (65535) and its `advance()` guard tests
/// `currentEdgeIndex_ < edges_.size()` *before* the increment, so a
/// freshly constructed iterator is already ended and the reference binary
/// yields no neighbours at all (the truth log's `ITER` lines all answer
/// count 0; its only consumer, `Navigator`'s radius search, never
/// iterates either). The port keeps the guard order and therefore the
/// same dead-on-arrival behaviour; the loop body is exercised by seeding
/// the cursor directly in the tests.
pub struct WaypointNeighbourIterator<'a> {
    set: &'a Rc<ChunkWaypointSetData>,
    waypoint: WaypointIndex,
    current_edge_index: EdgeIndex,
    neighbour_set: Option<Rc<ChunkWaypointSetData>>,
    neighbour_waypoint: WaypointIndex,
}

impl<'a> WaypointNeighbourIterator<'a> {
    /// The constructor (`waypoint_neighbour_iterator.cpp:12-21`).
    pub fn new(set: &'a Rc<ChunkWaypointSetData>, waypoint: WaypointIndex) -> Self {
        let mut iterator = Self {
            set,
            waypoint,
            current_edge_index: ADJACENT_RANGE_END,
            neighbour_set: None,
            neighbour_waypoint: OTHER_CHUNK,
        };
        iterator.advance();
        iterator
    }

    /// `advance()` (`waypoint_neighbour_iterator.cpp:27-70`).
    pub fn advance(&mut self) {
        self.neighbour_set = None;
        self.neighbour_waypoint = -1;

        let wp = &self.set.waypoints()[self.waypoint as usize];
        let edge_count = wp.edge_count as usize;

        while self.neighbour_set.is_none() && (self.current_edge_index as usize) < edge_count {
            self.current_edge_index = self.current_edge_index.wrapping_add(1);

            if self.current_edge_index as usize >= edge_count {
                break;
            }

            let cwe = self.set.edges()[wp.edge_range().start + self.current_edge_index as usize];
            let neighbour = cwe.neighbouring_waypoint();
            let adj_to_chunk = cwe.adjacent_to_chunk();

            if neighbour != -1 {
                self.neighbour_set = Some(Rc::clone(self.set));
                self.neighbour_waypoint = neighbour;
            } else if adj_to_chunk {
                let absolute = self
                    .set
                    .absolute_edge_index(self.waypoint as usize, self.current_edge_index as usize);
                if let Some(label) = self.set.edge_label(absolute)
                    && label.waypoint != -1
                    && let Some(portal) = self.set.connection_portal(&label.set)
                    && portal.permissive
                {
                    self.neighbour_set = Some(label.set);
                    self.neighbour_waypoint = label.waypoint;
                }
            }
        }
    }

    /// `ended()` (`waypoint_neighbour_iterator.hpp:30-35`).
    pub fn ended(&self) -> bool {
        let wp = &self.set.waypoints()[self.waypoint as usize];
        self.current_edge_index as usize >= wp.edge_count as usize
    }

    /// `pNeighbourSet()`.
    pub fn p_neighbour_set(&self) -> Option<&Rc<ChunkWaypointSetData>> {
        self.neighbour_set.as_ref()
    }

    /// `neighbourWaypointIndex()`.
    pub fn neighbour_waypoint_index(&self) -> WaypointIndex {
        self.neighbour_waypoint
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chunk_waypoint_set_data::parse_world_navmesh;

    /// One encoded set record: a single waypoint over the given edges.
    fn single_waypoint_set(edges: &[(f32, f32, i32)]) -> Rc<ChunkWaypointSetData> {
        let mut b = Vec::new();
        let put_f = |b: &mut Vec<u8>, v: f32| b.extend_from_slice(&v.to_le_bytes());
        let put_i = |b: &mut Vec<u8>, v: i32| b.extend_from_slice(&v.to_le_bytes());
        put_i(&mut b, 0);
        put_f(&mut b, 0.6);
        put_i(&mut b, 1);
        put_i(&mut b, edges.len() as i32);
        put_f(&mut b, 0.2);
        put_f(&mut b, 0.6);
        put_i(&mut b, edges.len() as i32);
        for (x, y, n) in edges {
            put_f(&mut b, *x);
            put_f(&mut b, *y);
            put_i(&mut b, *n);
        }
        let mut sets = parse_world_navmesh(&b).expect("decode");
        Rc::new(sets.remove(0))
    }

    const SQUARE: &[(f32, f32, i32)] = &[
        (0.0, 0.0, 65535),
        (0.0, 1.0, 65535),
        (1.0, 1.0, 65535),
        (1.0, 0.0, 65535),
    ];

    fn near_slice(left: &[f32], right: &[f32]) -> bool {
        left.len() == right.len() && left.iter().zip(right).all(|(a, b)| (a - b).abs() <= 1e-5)
    }

    #[test]
    fn the_constructor_face_builds_the_bind_probe_records() {
        let set = single_waypoint_set(SQUARE);
        set.build_neighbour_edges();

        let edges = set.neighbour_edges();
        assert_eq!(edges.len(), 4, "every edge is chunk-adjacent");
        assert_eq!(edges[0].0, 0);
        assert_eq!(edges[3].0, 3);

        // The three probe heights: the band centre (+0.1), the top plus
        // the tolerance and the bottom minus it.
        let entry = &edges[0].1;
        assert!(near_slice(&entry.y_values, &[0.5, 0.7, 0.1]));
        assert!(!entry.is_bound);

        // `v` is the midpoint between the edge's own vertex and the
        // next edge's vertex, height zero. The vertex table follows the
        // edge stream's first-appearance order, so edge 0 starts at
        // (0,0) and the next edge starts at (0,1): v = (0,0,0.5). Edge
        // 3 starts at (1,0) and wraps to edge 0's vertex (0,0):
        // v = (0.5,0,0).
        assert_eq!(entry.v, Vector3::new(0.0, 0.0, 0.5));
        assert_eq!(&edges[3].1.v, &Vector3::new(0.5, 0.0, 0.0));

        // The reference MF_ASSERTs a fresh index; building twice panics.
        assert!(
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                set.build_neighbour_edges();
            }))
            .is_err()
        );
    }

    #[test]
    fn the_constructor_face_skips_in_set_edges() {
        // wp0's edge 1 borders wp1 in-set; only edges 0/2/3 get records.
        let mut b = Vec::new();
        let put_f = |b: &mut Vec<u8>, v: f32| b.extend_from_slice(&v.to_le_bytes());
        let put_i = |b: &mut Vec<u8>, v: i32| b.extend_from_slice(&v.to_le_bytes());
        put_i(&mut b, 0);
        put_f(&mut b, 0.6);
        put_i(&mut b, 2);
        put_i(&mut b, 8);
        put_f(&mut b, 0.2);
        put_f(&mut b, 0.6);
        put_i(&mut b, 4);
        put_f(&mut b, 0.2);
        put_f(&mut b, 0.6);
        put_i(&mut b, 4);
        for (x, y, n) in [
            (0.0f32, 2.0, 65535),
            (1.0, 2.0, 1),
            (1.0, 0.0, 65535),
            (0.0, 0.0, 65535),
            (1.0, 2.0, 65535),
            (2.0, 2.0, 65535),
            (2.0, 0.0, 65535),
            (1.0, 0.0, 0),
        ] {
            put_f(&mut b, x);
            put_f(&mut b, y);
            put_i(&mut b, n);
        }
        let mut sets = parse_world_navmesh(&b).expect("decode");
        let set = Rc::new(sets.remove(0));
        set.build_neighbour_edges();

        let edges = set.neighbour_edges();
        assert_eq!(edges.len(), 6, "three boundary edges per waypoint");
        // wp0: edge 1 borders wp1 in-set, so edges 0/2/3 are boundary.
        // wp1: edge 3 (absolute 7) borders wp0 in-set, so edges 4/5/6 are.
        assert_eq!(
            edges.iter().map(|(e, _)| *e).collect::<Vec<_>>(),
            [0, 2, 3, 4, 5, 6]
        );
    }

    #[test]
    fn connect_records_the_first_connection_and_backlinks() {
        let a = single_waypoint_set(SQUARE);
        let b = single_waypoint_set(&[
            (1.0, 0.0, 65535),
            (1.0, 1.0, 65535),
            (2.0, 1.0, 65535),
            (2.0, 0.0, 65535),
        ]);
        let open = Rc::new(Portal {
            permissive: true,
            has_chunk_item: false,
            to_chunk: Some(1),
        });

        a.connect(
            Rc::clone(&b),
            0,
            Rc::clone(&open),
            2,
            Vector3::new(1.0, 0.5, 0.5),
        );

        let connections = a.connections();
        assert_eq!(connections.len(), 1);
        assert!(Rc::ptr_eq(&connections[0].0, &b));
        assert!(Rc::ptr_eq(&connections[0].1, &open));
        assert!(a.connection_portal(&b).is_some());
        let other = single_waypoint_set(&[(3.0, 3.0, 65535)]);
        assert!(a.connection_portal(&other).is_none());
        assert!(Rc::ptr_eq(&a.connection_waypoint(2).unwrap(), &b));
        assert!(a.connection_waypoint(0).is_none());

        let label = a.edge_label(2).expect("label");
        assert!(Rc::ptr_eq(&label.set, &b));
        assert_eq!(label.waypoint, 0);
        assert_eq!(label.entry_point, Vector3::new(1.0, 0.5, 0.5));
        assert!(a.edge_label(3).is_none(), "the miss answers none");

        // The backlink went to the neighbour.
        assert_eq!(b.backlinks().len(), 1);
        assert!(Rc::ptr_eq(&b.backlinks()[0], &a));

        // removeBacklink erases the first match; an unknown set only
        // prints the reference's ERROR_MSG.
        b.remove_backlink(&a);
        assert!(b.backlinks().is_empty());
        let again = Rc::clone(&a);
        b.remove_backlink(&again);
        assert!(b.backlinks().is_empty());
    }

    #[test]
    fn a_second_portal_only_notice_and_the_label_overwrites() {
        let a = single_waypoint_set(SQUARE);
        let b = single_waypoint_set(&[(1.0, 0.0, 65535)]);
        let first = Rc::new(Portal {
            permissive: true,
            has_chunk_item: false,
            to_chunk: Some(1),
        });
        let second = Rc::new(Portal {
            permissive: false,
            has_chunk_item: false,
            to_chunk: Some(1),
        });

        a.connect(
            Rc::clone(&b),
            0,
            Rc::clone(&first),
            2,
            Vector3::new(1.0, 0.5, 0.5),
        );
        // The same set through a different portal: NOTICE_MSG, the stored
        // portal stays, but the edge label overwrites unconditionally.
        a.connect(
            Rc::clone(&b),
            1,
            Rc::clone(&second),
            2,
            Vector3::new(1.0, 0.5, 0.7),
        );

        let connections = a.connections();
        assert_eq!(connections.len(), 1);
        assert!(Rc::ptr_eq(&connections[0].1, &first));
        let label = a.edge_label(2).unwrap();
        assert_eq!(label.waypoint, 1, "the label overwrote");
        assert_eq!(label.entry_point, Vector3::new(1.0, 0.5, 0.7));
        // connect added the backlink once (only on first sight).
        assert_eq!(b.backlinks().len(), 1);
    }

    #[test]
    fn delete_connection_clears_the_labels_backlink_and_record() {
        let a = single_waypoint_set(SQUARE);
        let b = single_waypoint_set(&[(1.0, 0.0, 65535)]);
        let open = Rc::new(Portal::default());
        a.build_neighbour_edges();
        a.connect(
            Rc::clone(&b),
            0,
            Rc::clone(&open),
            2,
            Vector3::new(1.0, 0.5, 0.5),
        );
        a.connect(
            Rc::clone(&b),
            0,
            Rc::clone(&open),
            3,
            Vector3::new(1.0, 0.5, 0.0),
        );

        // A label on a second set survives its own delete.
        let c = single_waypoint_set(&[(2.0, 2.0, 65535)]);
        a.connect(
            Rc::clone(&c),
            0,
            Rc::clone(&open),
            1,
            Vector3::new(0.0, 0.5, 1.0),
        );

        a.delete_connection(&b);
        assert!(a.edge_label(2).is_none());
        assert!(a.edge_label(3).is_none());
        assert!(a.edge_label(1).is_some(), "the other set's label stays");
        assert_eq!(a.connections().len(), 1);
        assert!(b.backlinks().is_empty());
        // The deleted edges' neighbour records came off isBound (they
        // were never bound here, so they stay false).
        assert!(a.neighbour_edges().iter().all(|(_, e)| !e.is_bound));

        // Deleting again is the reference's ERROR_MSG and a no-op.
        a.delete_connection(&b);
        assert_eq!(a.connections().len(), 1);
    }

    #[test]
    fn delete_connection_marks_the_neighbour_edge_unbound() {
        let a = single_waypoint_set(SQUARE);
        let b = single_waypoint_set(&[(1.0, 0.0, 65535)]);
        let open = Rc::new(Portal::default());
        a.build_neighbour_edges();
        a.connect(
            Rc::clone(&b),
            0,
            Rc::clone(&open),
            2,
            Vector3::new(1.0, 0.5, 0.5),
        );
        // bind()'s half of the story: the edge record went isBound.
        a.neighbour_edges.borrow_mut().get_mut(&2).unwrap().is_bound = true;

        a.delete_connection(&b);
        assert!(
            !a.neighbour_edges()
                .into_iter()
                .any(|(e, entry)| e == 2 && entry.is_bound)
        );
    }

    #[test]
    fn remove_our_connections_clears_everything_and_unbinds() {
        let a = single_waypoint_set(SQUARE);
        let b = single_waypoint_set(&[(1.0, 0.0, 65535)]);
        let open = Rc::new(Portal::default());
        a.build_neighbour_edges();
        a.connect(
            Rc::clone(&b),
            0,
            Rc::clone(&open),
            2,
            Vector3::new(1.0, 0.5, 0.5),
        );
        a.neighbour_edges.borrow_mut().get_mut(&2).unwrap().is_bound = true;

        a.remove_our_connections();
        assert!(a.edge_labels.borrow().is_empty());
        assert!(a.connections().is_empty());
        assert!(b.backlinks().is_empty());
        assert!(a.neighbour_edges().iter().all(|(_, e)| !e.is_bound));
    }

    #[test]
    fn remove_others_connections_deletes_through_the_linkers() {
        let a = single_waypoint_set(SQUARE);
        let b = single_waypoint_set(&[(1.0, 0.0, 65535)]);
        let c = single_waypoint_set(&[(2.0, 2.0, 65535)]);
        let open = Rc::new(Portal::default());
        b.build_neighbour_edges();
        b.connect(
            Rc::clone(&a),
            0,
            Rc::clone(&open),
            0,
            Vector3::new(1.0, 0.5, 0.0),
        );
        b.connect(
            Rc::clone(&c),
            0,
            Rc::clone(&open),
            0,
            Vector3::new(1.0, 0.5, 1.0),
        );
        // `connect` drops a backlink on the *target*, so the two linkers
        // are `a` and `c`; the connector `b` holds no backlinks.
        assert_eq!(a.backlinks().len(), 1);
        assert_eq!(c.backlinks().len(), 1);
        assert!(b.backlinks().is_empty());

        // Each linker tells `b` to drop the connection to it, which also
        // removes the backlink from that linker.
        a.remove_others_connections();
        c.remove_others_connections();
        assert!(a.backlinks().is_empty());
        assert!(c.backlinks().is_empty());
        assert!(b.connections().is_empty());
        assert!(b.edge_labels.borrow().is_empty());
    }

    #[test]
    fn delete_connection_tolerates_a_set_whose_edges_were_never_built() {
        // A set that never ran the constructor face (no neighbour-edge
        // records) can still carry labels; `deleteConnection` unbinds the
        // neighbour edges through a miss-tolerant lookup, matching the
        // reference's `neighbourEdges_` absent entry.
        let a = single_waypoint_set(SQUARE);
        let b = single_waypoint_set(&[(1.0, 0.0, 65535)]);
        let open = Rc::new(Portal::default());
        a.connect(
            Rc::clone(&b),
            0,
            Rc::clone(&open),
            2,
            Vector3::new(1.0, 0.5, 0.5),
        );
        assert!(a.edge_label(2).is_some());
        assert!(
            a.neighbour_edges().is_empty(),
            "the constructor face never ran"
        );

        a.delete_connection(&b);
        assert!(a.connections().is_empty());
        assert!(a.edge_label(2).is_none(), "the label was dropped");
        assert!(b.backlinks().is_empty(), "the link was removed");
    }

    #[test]
    fn a_backlink_without_a_connection_is_dropped_with_an_error() {
        let a = single_waypoint_set(SQUARE);
        let b = single_waypoint_set(&[(1.0, 0.0, 65535)]);
        // The stale backlink: never backed by a connection.
        a.add_backlink(Rc::clone(&b));

        a.remove_others_connections();
        assert!(a.backlinks().is_empty(), "the stale entry was dropped");
        assert!(b.connections().is_empty());
    }

    #[test]
    fn the_chunk_identity_and_outside_flags_round_trip() {
        let set = single_waypoint_set(SQUARE);
        assert_eq!(set.chunk_identity(), None);
        assert!(!set.is_outside_chunk());
        set.set_chunk_identity(7);
        set.set_outside_chunk(true);
        assert_eq!(set.chunk_identity(), Some(7));
        assert!(set.is_outside_chunk());
    }

    #[test]
    fn a_fresh_iterator_is_already_ended() {
        let set = single_waypoint_set(SQUARE);
        // The constructor seeds the cursor with adjacentRangeEnd and the
        // guard tests it before the increment: the reference walk never
        // runs (the truth log's ITER lines all answer count 0).
        let iterator = WaypointNeighbourIterator::new(&set, 0);
        assert!(iterator.ended());
        assert!(iterator.p_neighbour_set().is_none());
        assert_eq!(iterator.neighbour_waypoint_index(), -1);
    }

    #[test]
    fn the_seeded_walk_takes_in_set_edges_then_the_permissive_portal() {
        // The split square: wp0's edge 1 borders wp1 in-set, edges 2/3
        // are boundary.
        let mut b = Vec::new();
        let put_f = |b: &mut Vec<u8>, v: f32| b.extend_from_slice(&v.to_le_bytes());
        let put_i = |b: &mut Vec<u8>, v: i32| b.extend_from_slice(&v.to_le_bytes());
        put_i(&mut b, 0);
        put_f(&mut b, 0.6);
        put_i(&mut b, 2);
        put_i(&mut b, 8);
        put_f(&mut b, 0.2);
        put_f(&mut b, 0.6);
        put_i(&mut b, 4);
        put_f(&mut b, 0.2);
        put_f(&mut b, 0.6);
        put_i(&mut b, 4);
        for (x, y, n) in [
            (0.0f32, 2.0, 65535),
            (1.0, 2.0, 1),
            (1.0, 0.0, 65535),
            (0.0, 0.0, 65535),
            (1.0, 2.0, 65535),
            (2.0, 2.0, 65535),
            (2.0, 0.0, 65535),
            (1.0, 0.0, 0),
        ] {
            put_f(&mut b, x);
            put_f(&mut b, y);
            put_i(&mut b, n);
        }
        let mut sets = parse_world_navmesh(&b).expect("decode");
        let set = Rc::new(sets.remove(0));
        let other = single_waypoint_set(&[
            (1.0, 2.0, 65535),
            (2.0, 2.0, 65535),
            (2.0, 0.0, 65535),
            (1.0, 0.0, 65535),
        ]);
        let open = Rc::new(Portal {
            permissive: true,
            has_chunk_item: false,
            to_chunk: Some(1),
        });
        // The label on the boundary edge 2 (abs 2) points at `other`.
        set.connect(
            Rc::clone(&other),
            0,
            Rc::clone(&open),
            2,
            Vector3::new(1.0, 0.5, 1.0),
        );

        // Seeding the cursor at 0 (what the intended wrap would do)
        // walks: edge 1's in-set neighbour, then edge 2's permissive
        // portal crossing, then edge 3 refuses and the walk ends.
        let mut iterator = WaypointNeighbourIterator::new(&set, 0);
        iterator.current_edge_index = 0;
        iterator.advance();
        assert!(!iterator.ended());
        assert!(Rc::ptr_eq(iterator.p_neighbour_set().unwrap(), &set));
        assert_eq!(iterator.neighbour_waypoint_index(), 1, "the in-set link");

        iterator.advance();
        assert!(!iterator.ended());
        assert!(Rc::ptr_eq(iterator.p_neighbour_set().unwrap(), &other));
        assert_eq!(
            iterator.neighbour_waypoint_index(),
            0,
            "the portal crossing"
        );

        iterator.advance();
        assert!(iterator.ended(), "edge 3 carries no crossing");
    }

    #[test]
    fn the_seeded_walk_refuses_non_permissive_and_labelless_edges() {
        // Three boundary edges: e1 carries a vista encoding (its label
        // lookup misses), e2's label goes through the given portal with
        // the given waypoint.
        let make = |permissive: bool, waypoint: i32| {
            let set = single_waypoint_set(&[
                (0.0, 0.0, 65535),
                (0.0, 1.0, 65530), // vista encoding
                (1.0, 1.0, 65535),
            ]);
            let other = single_waypoint_set(&[(1.0, 0.0, 65535)]);
            let portal = Rc::new(Portal {
                permissive,
                has_chunk_item: false,
                to_chunk: Some(1),
            });
            set.connect(other, waypoint, portal, 2, Vector3::new(1.0, 0.5, 0.5));
            set
        };

        // A non-permissive portal refuses.
        let shut = make(false, 0);
        let mut iterator = WaypointNeighbourIterator::new(&shut, 0);
        iterator.current_edge_index = 0;
        iterator.advance();
        assert!(iterator.ended());
        assert!(iterator.p_neighbour_set().is_none());

        // A permissive portal passes.
        let open = make(true, 0);
        let mut iterator = WaypointNeighbourIterator::new(&open, 0);
        iterator.current_edge_index = 1;
        iterator.advance();
        assert!(!iterator.ended());
        assert!(iterator.p_neighbour_set().is_some());

        // A label whose waypoint is -1 refuses as well (the guard before
        // the portal check).
        let dead = make(true, -1);
        let mut iterator = WaypointNeighbourIterator::new(&dead, 0);
        iterator.current_edge_index = 1;
        iterator.advance();
        assert!(iterator.ended());
    }
}
