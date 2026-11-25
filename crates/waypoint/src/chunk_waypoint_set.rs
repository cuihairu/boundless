//! `lib/waypoint/chunk_waypoint_set.{hpp,cpp}` — the runtime connection
//! face over a decoded waypoint set: the portal-brokered connections to
//! neighbouring chunks' sets, the per-edge labels they were made through,
//! and the backlinks the neighbours keep.
//!
//! The reference wraps `ChunkWaypointSetData` in a `ChunkWaypointSet`
//! (chunk-mounted, refcounted) whose `bind()` walks the chunk's portals
//! and calls the private `connect()`; the port keeps the connection state
//! as `RefCell` fields on the data record itself (same crate, so the
//! inherent impls below are legal) and offers `connect()` as the wiring
//! face the replay harness drives with the reference `bind()`'s output.
//!
//! Registered boundaries (same answers, different bookkeeping):
//! - `connections_` is a `BW::map<ChunkWaypointSetPtr, Portal*>` ordered by
//!   the set pointer; the port keys a `BTreeMap` by the `Rc` address.
//! - `edgeLabels_` is a `BW::unordered_map` whose iteration order is
//!   allocation-dependent; the port's `BTreeMap` iterates ascending edge
//!   index. The one order-sensitive consumer (the set-level nearest-entry
//!   scan, a strict `<`) only differs on exact distance ties, and the
//!   fixture probes are placed to avoid them.
//! - `Portal` keeps just the two flags the waypoint faces consume
//!   (`permissive`, `hasChunkItem`); the geometry lives in the chunk lane.
//! - The chunk identity each set is mounted on (needed by the shell
//!   formula and the chunk-gone guard) becomes an optional id on the data
//!   record; unmounted is the reference's missing `chunk()`.

use super::chunk_waypoint_set_data::ChunkWaypointSetData;
use super::{ADJACENT_RANGE_END, EdgeIndex, WaypointIndex};
use foundation::math::Vector3;
use std::rc::Rc;

/// The two portal flags the waypoint faces read
/// (`chunk_boundary.hpp:186-280`): whether the portal may be crossed
/// freely (`permissive`) and whether a chunk item has claimed it
/// (`hasChunkItem`). The reference reads them off the full
/// `ChunkBoundary::Portal`; the port keeps only the flags.
#[derive(Debug, Clone, Default)]
pub struct Portal {
    /// `permissive`.
    pub permissive: bool,
    /// `hasChunkItem_`.
    pub has_chunk_item: bool,
}

/// `EdgeLabel` (`chunk_waypoint_set.hpp:27-32`): what `bind()` records for
/// one boundary edge — the neighbouring set, the waypoint it landed on and
/// the world-space point the connection was made through.
#[derive(Debug, Clone)]
pub struct EdgeLabel {
    /// `pSet_`.
    pub set: Rc<ChunkWaypointSetData>,
    /// `waypoint_` (`-1` never survives a real `bind()`).
    pub waypoint: WaypointIndex,
    /// `entryPoint_`.
    pub entry_point: Vector3,
}

impl ChunkWaypointSetData {
    /// `ChunkWaypointSet::connect` (`chunk_waypoint_set.cpp:293-323`):
    /// record a neighbouring set reached through `portal` over edge
    /// `edge_index`, landing on `waypoint` at `entry_point`. A set's first
    /// connection is inserted and the neighbour backlinks us
    /// (`pWaypointSet->addBacklink( this )`); the same set through a
    /// different portal is a `NOTICE` and leaves the stored portal alone;
    /// the edge label is overwritten unconditionally.
    pub fn connect(
        self: &Rc<Self>,
        set: Rc<ChunkWaypointSetData>,
        portal: Rc<Portal>,
        edge_index: EdgeIndex,
        waypoint: WaypointIndex,
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
                        // NOTICE_MSG in the reference: "Existing connection
                        // to the same chunk through a different portal".
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
    /// neighbouring set edge `edge_index` was labelled with.
    pub fn connection_waypoint(&self, edge_index: EdgeIndex) -> Option<Rc<ChunkWaypointSetData>> {
        self.edge_labels
            .borrow()
            .get(&edge_index)
            .map(|label| Rc::clone(&label.set))
    }

    /// `edgeLabel` (`chunk_waypoint_set.hpp:218-230`): the label recorded
    /// for an edge. The reference answers a static
    /// `{NULL, -1, Vector3()}` record on a miss; the port answers `None`
    /// (the `NULL pSet_` the callers test for).
    pub fn edge_label(&self, edge_index: EdgeIndex) -> Option<EdgeLabel> {
        self.edge_labels.borrow().get(&edge_index).cloned()
    }

    /// The connections in pointer order (`connectionsBegin`/`End` walking
    /// the pointer-keyed map): the neighbouring set and the portal.
    pub fn connections(&self) -> Vec<(Rc<ChunkWaypointSetData>, Rc<Portal>)> {
        self.connections
            .borrow()
            .values()
            .cloned()
            .collect::<Vec<_>>()
    }

    /// The edge labels in ascending edge-index order.
    pub fn edge_labels(&self) -> Vec<(EdgeIndex, EdgeLabel)> {
        self.edge_labels
            .borrow()
            .iter()
            .map(|(edge, label)| (*edge, label.clone()))
            .collect()
    }

    /// `addBacklink` (`chunk_waypoint_set.cpp:228-231`): a plain append —
    /// duplicates are the caller's problem, as in the reference.
    pub fn add_backlink(&self, set: Rc<ChunkWaypointSetData>) {
        self.backlinks.borrow_mut().push(set);
    }

    /// `removeBacklink` (`chunk_waypoint_set.cpp:239-253`): erase the
    /// first occurrence; a missing entry is the reference's ERROR_MSG and
    /// a no-op.
    pub fn remove_backlink(&self, set: &Rc<ChunkWaypointSetData>) {
        let mut backlinks = self.backlinks.borrow_mut();
        if let Some(position) = backlinks.iter().position(|other| Rc::ptr_eq(other, set)) {
            backlinks.remove(position);
        } else {
            eprintln!("ERROR_MSG: ChunkWaypointSet::removeBacklink: no such backlink");
        }
    }

    /// The backlink list (`backlinks_`), cloned out.
    pub fn backlinks(&self) -> Vec<Rc<ChunkWaypointSetData>> {
        self.backlinks.borrow().clone()
    }

    /// Mount the set on a chunk identity: the shell-boundary formula and
    /// the set-level chunk guard consume `chunk()` on the reference's
    /// mounted set. Ids are opaque (the fixture harness allots one per
    /// chunk).
    pub fn set_chunk_identity(&self, id: usize) {
        self.chunk_id.set(Some(id));
    }

    /// The mounted chunk identity (`chunk()`), or `None` while unmounted.
    pub fn chunk_identity(&self) -> Option<usize> {
        self.chunk_id.get()
    }

    /// `Chunk::isOutsideChunk` (`chunk.cpp:532-535`) as seen by the shell
    /// formula: the mounted chunk's outside flag. Defaults inside, the
    /// fixture map's shape.
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
/// `adjacentRangeEnd` (65535) and relies on the loop's `++` wrapping it to
/// 0 — but `advance()` tests `currentEdgeIndex_ < edges_.size()` *before*
/// the increment, so a freshly constructed iterator is already ended and
/// the reference binary yields no neighbours at all (its only consumer,
/// `Navigator`'s radius search, never iterates either). The port keeps
/// the guard order and therefore the same dead-on-arrival behaviour; the
/// loop body is exercised by seeding the cursor directly in the tests.
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
            neighbour_waypoint: super::chunk_waypoint::OTHER_CHUNK,
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
    use foundation::math::Vector3;

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

    #[test]
    fn connect_records_the_first_connection_and_backlinks() {
        let a = single_waypoint_set(&[
            (0.0, 0.0, 65535),
            (0.0, 1.0, 65535),
            (1.0, 1.0, 65535),
            (1.0, 0.0, 65535),
        ]);
        let b = single_waypoint_set(&[
            (1.0, 0.0, 65535),
            (1.0, 1.0, 65535),
            (2.0, 1.0, 65535),
            (2.0, 0.0, 65535),
        ]);
        let open = Rc::new(Portal {
            permissive: true,
            has_chunk_item: false,
        });

        a.connect(
            Rc::clone(&b),
            Rc::clone(&open),
            2,
            0,
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
        let a = single_waypoint_set(&[
            (0.0, 0.0, 65535),
            (0.0, 1.0, 65535),
            (1.0, 1.0, 65535),
            (1.0, 0.0, 65535),
        ]);
        let b = single_waypoint_set(&[(1.0, 0.0, 65535)]);
        let first = Rc::new(Portal {
            permissive: true,
            has_chunk_item: false,
        });
        let second = Rc::new(Portal {
            permissive: false,
            has_chunk_item: false,
        });

        a.connect(
            Rc::clone(&b),
            Rc::clone(&first),
            2,
            0,
            Vector3::new(1.0, 0.5, 0.5),
        );
        // The same set through a different portal: NOTICE_MSG, the stored
        // portal stays, but the edge label overwrites unconditionally.
        a.connect(
            Rc::clone(&b),
            Rc::clone(&second),
            2,
            1,
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
    fn the_chunk_identity_and_outside_flags_round_trip() {
        let set = single_waypoint_set(&[(0.0, 0.0, 65535)]);
        assert_eq!(set.chunk_identity(), None);
        assert!(!set.is_outside_chunk());
        set.set_chunk_identity(7);
        set.set_outside_chunk(true);
        assert_eq!(set.chunk_identity(), Some(7));
        assert!(set.is_outside_chunk());
    }

    #[test]
    fn a_fresh_iterator_is_already_ended() {
        let set = single_waypoint_set(&[
            (0.0, 0.0, 65535),
            (0.0, 1.0, 65535),
            (1.0, 1.0, 65535),
            (1.0, 0.0, 65535),
        ]);
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
        });
        // The label on the boundary edge 2 (abs 2) points at `other`.
        set.connect(
            Rc::clone(&other),
            Rc::clone(&open),
            2,
            0,
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
            });
            set.connect(other, portal, 2, waypoint, Vector3::new(1.0, 0.5, 0.5));
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
