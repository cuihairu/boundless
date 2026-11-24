//! BW-10-04-05 — the cross-chunk waypoint set search, replayed against
//! the original engine's own output.
//!
//! The fixture (`docs/review/fixtures/waypoint/gen_waypoints_cross.cpp`)
//! links the reference el7 libraries and assembles a real three-chunk
//! world: three inside chunks inside the unit box, the original
//! `ChunkWaypointSet::bind()` creating every connection/edge label, and
//! the original A* answering the probes. Its truth log
//! (`gen_waypoints_cross.log`) is what these tests pin:
//! - the decode (HDR/SET/WP/EDGE),
//! - the bind output (PORTAL/CONNTO/EDGELABEL) re-created through the
//!   port's `connect()` and then re-read back,
//! - the neighbour iterator (ITER — the reference iterator is
//!   dead-on-arrival: its constructor seeds the cursor past the loop
//!   guard, so every walk is empty),
//! - the chunk-level A* crossing one portal (XPATH),
//! - the set-level A* over the portal connections (SETSEARCH/SETHOP) —
//!   the portal access order.
//!
//! BW-10-04-06/07 add the navigator faces on the same truth: the
//! findPath probes (FP) and the maxSearchDistance sweep (DOK/DPNONE, plus
//! DCACHE — a changed limit on one navigator).

use foundation::math::Vector3;
use std::rc::Rc;
use waypoint::{
    AStar, CacheState, ChunkWPSetState, ChunkWaypointState, NavLoc, Navigator, Portal,
    WaypointNeighbourIterator, parse_world_navmesh,
};

const FIXTURE_DIR: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../docs/review/fixtures/waypoint"
);

fn fixture_bytes(name: &str) -> Vec<u8> {
    std::fs::read(format!("{FIXTURE_DIR}/{name}")).expect("fixture")
}

/// The world the reference `bind()` produced: three sets on chunks
/// A/B/C, the four portals (A|B non-permissive both ways, B|C permissive
/// both ways), and every `connect()` the log's CONNTO/EDGELABEL lines
/// report. Returns the sets and the portals so tests can compare portal
/// identities.
fn wired_world() -> (Vec<Rc<waypoint::ChunkWaypointSetData>>, [Rc<Portal>; 4]) {
    let sets: Vec<Rc<waypoint::ChunkWaypointSetData>> =
        parse_world_navmesh(&fixture_bytes("world_navmesh_cross.bin"))
            .expect("the fixture decodes")
            .into_iter()
            .map(Rc::new)
            .collect();
    assert_eq!(sets.len(), 3, "the log's three SET lines");

    for (chunk_id, set) in sets.iter().enumerate() {
        set.set_chunk_identity(chunk_id);
    }
    let [a, b, c] = [sets[0].clone(), sets[1].clone(), sets[2].clone()];

    let non_permissive = Rc::new(Portal {
        permissive: false,
        has_chunk_item: false,
    });
    let permissive = Rc::new(Portal {
        permissive: true,
        has_chunk_item: false,
    });
    let (p0, p1) = (Rc::clone(&non_permissive), Rc::clone(&non_permissive));
    let (p2, p3) = (Rc::clone(&permissive), Rc::clone(&permissive));

    // CONNTO/EDGELABEL: A --p0--> B over edge 2; B --p1--> A over edges
    // 0 and 4; B --p2--> C over edges 2 and 6; C --p3--> B over edge 0.
    // The entry points are the bind probe points (the west/east edge
    // midpoints at the first bind height, 0.5).
    let third = 1.0f32 / 3.0;
    let two_thirds = 2.0f32 / 3.0;
    a.connect(b.clone(), p0.clone(), 2, 0, Vector3::new(third, 0.5, 0.5));
    b.connect(a.clone(), p1.clone(), 0, 0, Vector3::new(third, 0.5, 0.25));
    b.connect(a.clone(), p1.clone(), 4, 0, Vector3::new(third, 0.5, 0.75));
    b.connect(
        c.clone(),
        p2.clone(),
        2,
        0,
        Vector3::new(two_thirds, 0.5, 0.25),
    );
    b.connect(
        c.clone(),
        p2.clone(),
        6,
        0,
        Vector3::new(two_thirds, 0.5, 0.75),
    );
    c.connect(
        b.clone(),
        p3.clone(),
        0,
        0,
        Vector3::new(two_thirds, 0.5, 0.5),
    );

    (sets, [p0, p1, p2, p3])
}

fn near(a: f32, b: f32) -> bool {
    (a - b).abs() <= 1e-5
}

fn point_near(p: Vector3, x: f32, y: f32, z: f32) -> bool {
    near(p.x, x) && near(p.y, y) && near(p.z, z)
}

/// A navloc on a set at the truth log's (already height-lifted) probe
/// position and waypoint.
fn loc(set: &Rc<waypoint::ChunkWaypointSetData>, waypoint: i32, x: f32, y: f32, z: f32) -> NavLoc {
    NavLoc::from_parts(set.clone(), waypoint, Vector3::new(x, y, z), 100.0)
}

#[test]
fn the_fixture_decodes_three_sets_as_logged() {
    let sets: Vec<_> = parse_world_navmesh(&fixture_bytes("world_navmesh_cross.bin"))
        .expect("the fixture decodes");

    // HDR/SET: girth 0.6, bases 1/2/4, waypoint counts 1/2/1.
    assert_eq!(sets[0].base_index(), 1);
    assert_eq!(sets[1].base_index(), 2);
    assert_eq!(sets[2].base_index(), 4);
    assert_eq!(sets[0].waypoints().len(), 1);
    assert_eq!(sets[1].waypoints().len(), 2);
    assert_eq!(sets[2].waypoints().len(), 1);
    for set in &sets {
        assert!(near(set.girth(), 0.6));
    }

    // WP: heights 0.2..0.6, centres as logged.
    let wp = &sets[0].waypoints()[0];
    assert!(near(wp.min_height, 0.2) && near(wp.max_height, 0.6));
    // The log prints centre 0.166667 0.500000: the length-weighted
    // midpoint average in (x, z).
    assert!(near(wp.centre.x, 0.166667), "{:?}", wp.centre);
    assert!(near(wp.centre.y, 0.5), "{:?}", wp.centre);

    // EDGE: the split strip's reciprocal in-set links (0001/0000) and the
    // boundary edges (FFFF) as logged.
    let b_edges = &sets[1].edges()[sets[1].waypoints()[0].edge_range().clone()];
    assert_eq!(b_edges[1].neighbour, 1, "B0 north borders B1");
    assert_eq!(b_edges[3].neighbour, u16::MAX);
    let b1_edges = &sets[1].edges()[sets[1].waypoints()[1].edge_range().clone()];
    assert_eq!(b1_edges[3].neighbour, 0, "B1 south borders B0");
}

#[test]
fn the_bind_wiring_restores_connections_and_labels() {
    let (sets, portals) = wired_world();
    let [a, b, c] = [&sets[0], &sets[1], &sets[2]];
    let [p0, p1, p2, p3] = &portals;
    let third = 1.0f32 / 3.0;
    let two_thirds = 2.0f32 / 3.0;

    // CONNTO: A→B p0; B→A p1, B→C p2; C→B p3 (the destination sets and
    // the portal identities; the reference's listing order is its
    // pointer order and is not part of the contract).
    let a_conns = a.connections();
    assert_eq!(a_conns.len(), 1);
    assert!(Rc::ptr_eq(&a_conns[0].0, b));
    assert!(Rc::ptr_eq(&a_conns[0].1, p0));

    let b_conns = b.connections();
    assert_eq!(b_conns.len(), 2);
    assert!(
        b_conns
            .iter()
            .any(|(set, portal)| Rc::ptr_eq(set, a) && Rc::ptr_eq(portal, p1))
    );
    assert!(
        b_conns
            .iter()
            .any(|(set, portal)| Rc::ptr_eq(set, c) && Rc::ptr_eq(portal, p2))
    );

    let c_conns = c.connections();
    assert_eq!(c_conns.len(), 1);
    assert!(Rc::ptr_eq(&c_conns[0].0, b));
    assert!(Rc::ptr_eq(&c_conns[0].1, p3));

    // EDGELABEL: every label's set/waypoint/entry point as logged, and
    // the same-set different-portal call left the stored portals alone.
    let label = |set: &Rc<waypoint::ChunkWaypointSetData>, edge: u16| {
        let label = set
            .edge_label(edge)
            .unwrap_or_else(|| panic!("edge {edge}"));
        (label.set.clone(), label.waypoint, label.entry_point)
    };
    let (set, wp, entry) = label(a, 2);
    assert!(Rc::ptr_eq(&set, b));
    assert_eq!(wp, 0);
    assert!(point_near(entry, third, 0.5, 0.5));

    let (set, wp, entry) = label(b, 0);
    assert!(Rc::ptr_eq(&set, a));
    assert_eq!(wp, 0);
    assert!(point_near(entry, third, 0.5, 0.25));
    let (set, wp, entry) = label(b, 4);
    assert!(Rc::ptr_eq(&set, a));
    assert_eq!(wp, 0);
    assert!(point_near(entry, third, 0.5, 0.75));
    let (set, wp, entry) = label(b, 2);
    assert!(Rc::ptr_eq(&set, c));
    assert_eq!(wp, 0);
    assert!(point_near(entry, two_thirds, 0.5, 0.25));
    let (set, wp, entry) = label(b, 6);
    assert!(Rc::ptr_eq(&set, c));
    assert_eq!(wp, 0);
    assert!(point_near(entry, two_thirds, 0.5, 0.75));

    let (set, wp, entry) = label(c, 0);
    assert!(Rc::ptr_eq(&set, b));
    assert_eq!(wp, 0);
    assert!(point_near(entry, two_thirds, 0.5, 0.5));

    // The backlinks `connect` added on first sight (one per neighbour).
    assert_eq!(a.backlinks().len(), 1);
    assert_eq!(b.backlinks().len(), 2);
    assert_eq!(c.backlinks().len(), 1);
}

#[test]
fn the_neighbour_iterator_matches_the_reference_walk() {
    let (sets, _) = wired_world();

    // ITER 0 0 / 1 0 / 1 1 / 2 0 all answer (count 0): the reference
    // iterator seeds its cursor with adjacentRangeEnd and its loop guard
    // runs before the increment, so a fresh walk is already ended.
    for (set_index, waypoint) in [(0usize, 0i32), (1, 0), (1, 1), (2, 0)] {
        let iterator = WaypointNeighbourIterator::new(&sets[set_index], waypoint);
        assert!(
            iterator.ended(),
            "ITER {set_index} {waypoint}: the reference walk is empty"
        );
        assert!(iterator.p_neighbour_set().is_none());
        assert_eq!(iterator.neighbour_waypoint_index(), -1);
    }
}

/// The chunk-level XPATH probes, straight off the log.
#[test]
fn the_chunk_level_paths_cross_the_portals_as_logged() {
    let (sets, _) = wired_world();
    let [a, b, c] = [&sets[0], &sets[1], &sets[2]];
    let third = 1.0f32 / 3.0;
    let two_thirds = 2.0f32 / 3.0;

    let run = |src: (&Rc<waypoint::ChunkWaypointSetData>, i32, f32, f32, f32),
               dst: (&Rc<waypoint::ChunkWaypointSetData>, i32, f32, f32, f32)| {
        let src = ChunkWaypointState::from_navloc(&loc(src.0, src.1, src.2, src.3, src.4));
        let dst = ChunkWaypointState::from_navloc(&loc(dst.0, dst.1, dst.2, dst.3, dst.4));
        let mut astar = AStar::default();
        let found = astar.search(&src, &dst, -1.0);
        (found, astar.path().to_vec())
    };

    // XPATH AB: two states, the crossing at (1/3, 0.6, 0.321429) on B's
    // waypoint 0.
    let (found, path) = run((a, 0, 0.15, 0.41, 0.4), (b, 0, 0.5, 0.41, 0.25));
    assert!(found);
    assert_eq!(path.len(), 2);
    assert_eq!(path[0].nav_loc().waypoint(), 0);
    assert!(Rc::ptr_eq(path[0].nav_loc().p_set().unwrap(), a));
    let p = path[1].nav_loc().point();
    assert!(point_near(p, third, 0.6, 0.321429), "{p:?}");
    assert_eq!(path[1].nav_loc().waypoint(), 0);
    assert!(Rc::ptr_eq(path[1].nav_loc().p_set().unwrap(), b));

    // XPATH BA: the mirror crossing.
    let (found, path) = run((b, 0, 0.5, 0.41, 0.25), (a, 0, 0.15, 0.41, 0.4));
    assert!(found);
    assert_eq!(path.len(), 2);
    let p = path[1].nav_loc().point();
    assert!(point_near(p, third, 0.6, 0.321429), "{p:?}");
    assert!(Rc::ptr_eq(path[1].nav_loc().p_set().unwrap(), a));

    // XPATH BC: the east pair, crossing at (2/3, 0.6, 0.369048).
    let (found, path) = run((b, 0, 0.5, 0.41, 0.25), (c, 0, 0.85, 0.41, 0.5));
    assert!(found);
    assert_eq!(path.len(), 2);
    let p = path[1].nav_loc().point();
    assert!(point_near(p, two_thirds, 0.6, 0.369048), "{p:?}");
    assert!(Rc::ptr_eq(path[1].nav_loc().p_set().unwrap(), c));

    // XPATH CB / AC: one crossing only — C's west label lands on B's
    // waypoint 0 but B1 (the goal) is a second hop away, and A→C is two
    // portals; the crossed-portal state refuses further expansion.
    let (found, _) = run((c, 0, 0.85, 0.41, 0.5), (b, 1, 0.5, 0.41, 0.75));
    assert!(!found, "XPATHNONE CB");
    let (found, _) = run((a, 0, 0.15, 0.41, 0.4), (c, 0, 0.85, 0.41, 0.5));
    assert!(!found, "XPATHNONE AC");

    // XPATH AA: the goal waypoint is the start's own.
    let (found, path) = run((a, 0, 0.15, 0.41, 0.4), (a, 0, 0.2, 0.41, 0.6));
    assert!(found);
    assert_eq!(path.len(), 1);
}

/// The SETSEARCH probes: the set-level A* and the portal access order —
/// the acceptance criterion's「同图 Portal 访问顺序吻合」.
#[test]
fn the_set_level_search_matches_the_portal_access_order() {
    let (sets, portals) = wired_world();
    let [a, b, c] = [&sets[0], &sets[1], &sets[2]];
    let [p0, p1, p2, p3] = &portals;
    let third = 1.0f32 / 3.0;
    let two_thirds = 2.0f32 / 3.0;

    let run = |src: (&Rc<waypoint::ChunkWaypointSetData>, i32, f32, f32, f32),
               dst: (&Rc<waypoint::ChunkWaypointSetData>, i32, f32, f32, f32),
               block_non_permissive: bool| {
        let mut src = ChunkWPSetState::from_navloc(&loc(src.0, src.1, src.2, src.3, src.4));
        let dst = ChunkWPSetState::from_navloc(&loc(dst.0, dst.1, dst.2, dst.3, dst.4));
        src.set_block_non_permissive(block_non_permissive);
        let mut astar = AStar::default();
        let found = astar.search(&src, &dst, -1.0);
        (found, astar.path().to_vec())
    };

    // SETHOP AC0: A → B → C through portals 0 and 2; the shell flag rides
    // both crossings and B's position is the label entry nearest the
    // start state's position.
    let (found, path) = run((a, 0, 0.15, 0.41, 0.4), (c, 0, 0.85, 0.41, 0.5), false);
    assert!(found);
    assert_eq!(path.len(), 3);
    assert!(Rc::ptr_eq(path[0].p_set().unwrap(), a));
    assert!(Rc::ptr_eq(path[1].p_set().unwrap(), b));
    assert!(Rc::ptr_eq(path[2].p_set().unwrap(), c));
    assert!(point_near(path[0].position(), 0.15, 0.41, 0.4));
    assert!(
        point_near(path[1].position(), third, 0.5, 0.25),
        "{:?}",
        path[1].position()
    );
    assert!(
        point_near(path[2].position(), 0.85, 0.41, 0.5),
        "the goal state keeps the goal position"
    );
    assert!(!path[0].passed_shell_boundary());
    assert!(path[1].passed_shell_boundary());
    assert!(path[2].passed_shell_boundary());
    assert!(!path[1].passed_activated_portal());
    let hop0 = a.connection_portal(path[1].p_set().unwrap()).unwrap();
    let hop1 = b.connection_portal(path[2].p_set().unwrap()).unwrap();
    assert!(Rc::ptr_eq(&hop0, p0), "AC0 first portal is 0");
    assert!(Rc::ptr_eq(&hop1, p2), "AC0 second portal is 2");

    // SETSEARCHNONE AC1: blocking non-permissive portals leaves A's only
    // connection (portal 0) unusable.
    let (found, _) = run((a, 0, 0.15, 0.41, 0.4), (c, 0, 0.85, 0.41, 0.5), true);
    assert!(!found, "SETSEARCHNONE AC1");

    // SETHOP CA0: C → B → A through portals 3 and 1.
    let (found, path) = run((c, 0, 0.85, 0.41, 0.6), (a, 0, 0.15, 0.41, 0.4), false);
    assert!(found);
    assert_eq!(path.len(), 3);
    assert!(Rc::ptr_eq(path[1].p_set().unwrap(), b));
    assert!(
        point_near(path[1].position(), two_thirds, 0.5, 0.75),
        "{:?}",
        path[1].position()
    );
    let hop0 = c.connection_portal(path[1].p_set().unwrap()).unwrap();
    let hop1 = b.connection_portal(path[2].p_set().unwrap()).unwrap();
    assert!(Rc::ptr_eq(&hop0, p3), "CA0 first portal is 3");
    assert!(Rc::ptr_eq(&hop1, p1), "CA0 second portal is 1");

    // SETSEARCHNONE AB1: the A|B pair is non-permissive in both
    // directions.
    let (found, _) = run((a, 0, 0.15, 0.41, 0.4), (b, 0, 0.5, 0.41, 0.25), true);
    assert!(!found, "SETSEARCHNONE AB1");

    // SETHOP BC0: B → C straight through portal 2.
    let (found, path) = run((b, 0, 0.5, 0.41, 0.25), (c, 0, 0.85, 0.41, 0.5), false);
    assert!(found);
    assert_eq!(path.len(), 2);
    assert!(point_near(path[0].position(), 0.5, 0.41, 0.25));
    assert!(point_near(path[1].position(), 0.85, 0.41, 0.5));
    let hop0 = b.connection_portal(path[1].p_set().unwrap()).unwrap();
    assert!(Rc::ptr_eq(&hop0, p2), "BC0 portal is 2");
}

/// One `findPath` probe: the two endpoints' sets, waypoints and points,
/// plus the non-permissive block flag.
struct Probe<'a> {
    src_set: &'a Rc<waypoint::ChunkWaypointSetData>,
    src_wp: i32,
    src: Vector3,
    dst_set: &'a Rc<waypoint::ChunkWaypointSetData>,
    dst_wp: i32,
    dst: Vector3,
    block: bool,
}

/// The findPath probes (`FPBEGIN`/`FPOK`/`FPNONE`/`FPEND`): the original
/// navigator's answer over the same wired world, with the acceptance pair
/// (AC0, its repeat) and the closed-portal failures (AC1/CA1/AB1).
#[test]
fn the_navigator_answers_the_reference_findpath_probes() {
    let (sets, _) = wired_world();
    let [a, b, c] = [&sets[0], &sets[1], &sets[2]];
    let third = 1.0f32 / 3.0;
    let two_thirds = 2.0f32 / 3.0;

    // AC0, AC0R (the cache hit), AC1, CA0, CA1, AB0, AB1, CB0, CB1, AA0.
    let probes = [
        Probe {
            src_set: a,
            src_wp: 0,
            src: Vector3::new(0.15, 0.41, 0.4),
            dst_set: c,
            dst_wp: 0,
            dst: Vector3::new(0.85, 0.41, 0.5),
            block: false,
        },
        Probe {
            src_set: a,
            src_wp: 0,
            src: Vector3::new(0.15, 0.41, 0.4),
            dst_set: c,
            dst_wp: 0,
            dst: Vector3::new(0.85, 0.41, 0.5),
            block: false,
        },
        Probe {
            src_set: a,
            src_wp: 0,
            src: Vector3::new(0.15, 0.41, 0.4),
            dst_set: c,
            dst_wp: 0,
            dst: Vector3::new(0.85, 0.41, 0.5),
            block: true,
        },
        Probe {
            src_set: c,
            src_wp: 0,
            src: Vector3::new(0.85, 0.41, 0.6),
            dst_set: a,
            dst_wp: 0,
            dst: Vector3::new(0.15, 0.41, 0.4),
            block: false,
        },
        Probe {
            src_set: c,
            src_wp: 0,
            src: Vector3::new(0.85, 0.41, 0.6),
            dst_set: a,
            dst_wp: 0,
            dst: Vector3::new(0.15, 0.41, 0.4),
            block: true,
        },
        Probe {
            src_set: a,
            src_wp: 0,
            src: Vector3::new(0.15, 0.41, 0.4),
            dst_set: b,
            dst_wp: 0,
            dst: Vector3::new(0.5, 0.41, 0.25),
            block: false,
        },
        Probe {
            src_set: a,
            src_wp: 0,
            src: Vector3::new(0.15, 0.41, 0.4),
            dst_set: b,
            dst_wp: 0,
            dst: Vector3::new(0.5, 0.41, 0.25),
            block: true,
        },
        Probe {
            src_set: c,
            src_wp: 0,
            src: Vector3::new(0.85, 0.41, 0.5),
            dst_set: b,
            dst_wp: 1,
            dst: Vector3::new(0.5, 0.41, 0.75),
            block: false,
        },
        Probe {
            src_set: c,
            src_wp: 0,
            src: Vector3::new(0.85, 0.41, 0.5),
            dst_set: b,
            dst_wp: 1,
            dst: Vector3::new(0.5, 0.41, 0.75),
            block: true,
        },
        Probe {
            src_set: a,
            src_wp: 0,
            src: Vector3::new(0.15, 0.41, 0.4),
            dst_set: a,
            dst_wp: 0,
            dst: Vector3::new(0.2, 0.41, 0.6),
            block: false,
        },
    ];

    let navigator = Navigator::default();
    let answers: Vec<_> = probes
        .iter()
        .map(|probe| {
            let src = loc(
                probe.src_set,
                probe.src_wp,
                probe.src.x,
                probe.src.y,
                probe.src.z,
            );
            let dst = loc(
                probe.dst_set,
                probe.dst_wp,
                probe.dst.x,
                probe.dst.y,
                probe.dst.z,
            );
            let mut next = NavLoc::new();
            let mut passed_activated_portal = false;
            let found = navigator.find_path(
                &src,
                &dst,
                1_000_000.0,
                probe.block,
                &mut next,
                &mut passed_activated_portal,
                CacheState::ReadWriteCache,
            );
            (found, next, passed_activated_portal, dst)
        })
        .collect();

    // FPOK AC0 0.333333 0.600000 0.426190 0 1 0 — and AC0R answers the
    // same, off the cached path.
    for (index, (found, next, act, _)) in [&answers[0], &answers[1]].into_iter().enumerate() {
        assert!(*found, "FPOK AC0 (probe {index})");
        assert!(
            point_near(next.point(), third, 0.6, 0.426190),
            "{:?}",
            next.point()
        );
        assert_eq!(next.waypoint(), 0);
        assert!(Rc::ptr_eq(next.p_set().unwrap(), b));
        assert!(!*act, "the reference's act flag stays 0");
    }

    // FPNONE AC1 / CA1 / AB1: blocking the non-permissive portals seals the
    // only routes off A.
    for (index, (found, next, _act, _dst)) in [&answers[2], &answers[4], &answers[6]]
        .into_iter()
        .enumerate()
    {
        assert!(!found, "FPNONE (probe {index})");
        assert!(next.point().y == 0.0, "the untouched next stays invalid");
    }

    // FPOK CA0 0.666667 0.600000 0.500000 0 1 0 — the mirror crossing.
    let (found, next, act, _) = &answers[3];
    assert!(*found, "FPOK CA0");
    assert!(
        point_near(next.point(), two_thirds, 0.6, 0.5),
        "{:?}",
        next.point()
    );
    assert_eq!(next.waypoint(), 0);
    assert!(Rc::ptr_eq(next.p_set().unwrap(), b));
    assert!(!*act);

    // FPOK AB0 0.500000 0.410000 0.250000 0 1 0: the destination itself is
    // the next waypoint along.
    let (found, next, act, _) = &answers[5];
    assert!(*found, "FPOK AB0");
    assert!(
        point_near(next.point(), 0.5, 0.41, 0.25),
        "{:?}",
        next.point()
    );
    assert_eq!(next.waypoint(), 0);
    assert!(Rc::ptr_eq(next.p_set().unwrap(), b));
    assert!(!*act);

    // FPOK CB0 / CB1: the permissive B|C pair is not blocked, so both
    // answer.
    for index in [7usize, 8] {
        let (found, next, act, _) = &answers[index];
        assert!(*found, "FPOK CB{index}");
        assert!(
            point_near(next.point(), two_thirds, 0.6, 0.5),
            "{:?}",
            next.point()
        );
        assert_eq!(next.waypoint(), 0);
        assert!(Rc::ptr_eq(next.p_set().unwrap(), b));
        assert!(!*act);
    }

    // FPOK AA0 0.200000 0.410000 0.600000 0 0 0: one set, the destination.
    let (found, next, _act, dst) = &answers[9];
    assert!(*found, "FPOK AA0");
    assert!(Rc::ptr_eq(next.p_set().unwrap(), a));
    assert!(point_near(
        next.point(),
        dst.point().x,
        dst.point().y,
        dst.point().z
    ));
}

/// The probe endpoints, keyed by the D-section pair name: source
/// (set, waypoint, point) then destination.
fn endpoints<'a>(
    name: &str,
    a: &'a Rc<waypoint::ChunkWaypointSetData>,
    b: &'a Rc<waypoint::ChunkWaypointSetData>,
    c: &'a Rc<waypoint::ChunkWaypointSetData>,
) -> (
    &'a Rc<waypoint::ChunkWaypointSetData>,
    i32,
    Vector3,
    &'a Rc<waypoint::ChunkWaypointSetData>,
    i32,
    Vector3,
) {
    match name {
        "AA" => (
            a,
            0,
            Vector3::new(0.15, 0.41, 0.4),
            a,
            0,
            Vector3::new(0.2, 0.41, 0.6),
        ),
        "AB" => (
            a,
            0,
            Vector3::new(0.15, 0.41, 0.4),
            b,
            0,
            Vector3::new(0.5, 0.41, 0.25),
        ),
        "AC" => (
            a,
            0,
            Vector3::new(0.15, 0.41, 0.4),
            c,
            0,
            Vector3::new(0.85, 0.41, 0.5),
        ),
        "CA" => (
            c,
            0,
            Vector3::new(0.85, 0.41, 0.6),
            a,
            0,
            Vector3::new(0.15, 0.41, 0.4),
        ),
        "BC" => (
            b,
            0,
            Vector3::new(0.5, 0.41, 0.25),
            c,
            0,
            Vector3::new(0.85, 0.41, 0.5),
        ),
        "BB" => (
            b,
            0,
            Vector3::new(0.5, 0.41, 0.25),
            b,
            1,
            Vector3::new(0.5, 0.41, 0.75),
        ),
        "AE" => (
            a,
            0,
            Vector3::new(0.15, 0.41, 0.4),
            b,
            0,
            Vector3::new(0.4, 0.41, 0.36),
        ),
        _ => unreachable!("unknown pair {name}"),
    }
}

/// The maxSearchDistance probes (`DOK`/`DPNONE`): the original navigator's
/// answer at limits straddling the two original thresholds — the A* filter
/// `distanceToGoal(start) > maxDistance` (`astar.ipp:125-131`) and the
/// in-set clamp `maxSearchDistance > src.gridSize()`
/// (`navigator.cpp:455-460`, the probes carry a 0.6 grid). The x-z flips
/// sit exactly on a pair distance (the set-level goal candidate carries
/// the dst point as its position):
///   AB sqrt(0.145)=0.380789, AC sqrt(0.50)=0.707107,
///   CA sqrt(0.53)=0.728010, BC sqrt(0.185)=0.430116;
/// BB/AE flip on the in-set crossing candidate instead (0.3140064 /
/// 0.26565), with the set level already green — the in-set filter is the
/// binder there. A fresh `Navigator` per row, like the fixture.
#[test]
fn the_navigator_answers_the_reference_distance_probes() {
    let (sets, _) = wired_world();
    let (a, b, c) = (sets[0].clone(), sets[1].clone(), sets[2].clone());

    // (pair, limit, ok, next x/y/z, next waypoint, next set) — the truth
    // log's `DOK`/`DPNONE` lines in order, one flat row per log line; the
    // next fields are 0 when the log answers DPNONE. The limits are the
    // fixture's own literals, which happen to echo 1/sqrt(2) once rounded.
    #[allow(clippy::approx_constant, clippy::type_complexity)] // the flat log row
    let rows: &[(&str, f32, bool, f32, f32, f32, i32, usize)] = &[
        // AA: one set, one waypoint — the start isGoal's the goal outright
        // (both the goal-only and the waypoint-equal test answer on set
        // identity), so no limit blocks it, not even 0.001.
        ("AA", -1.0, true, 0.2, 0.41, 0.6, 0, 0),
        ("AA", 0.0, true, 0.2, 0.41, 0.6, 0, 0),
        ("AA", 0.001, true, 0.2, 0.41, 0.6, 0, 0),
        ("AA", 0.05, true, 0.2, 0.41, 0.6, 0, 0),
        ("AA", 0.6, true, 0.2, 0.41, 0.6, 0, 0),
        ("AA", 2.0, true, 0.2, 0.41, 0.6, 0, 0),
        // AB: flips exactly between 0.3807 and 0.3808.
        ("AB", -1.0, true, 0.5, 0.41, 0.25, 0, 1),
        ("AB", 0.0, true, 0.5, 0.41, 0.25, 0, 1),
        ("AB", 0.3, false, 0.0, 0.0, 0.0, 0, 0),
        ("AB", 0.35, false, 0.0, 0.0, 0.0, 0, 0),
        ("AB", 0.3807, false, 0.0, 0.0, 0.0, 0, 0),
        ("AB", 0.3808, true, 0.5, 0.41, 0.25, 0, 1),
        ("AB", 0.381, true, 0.5, 0.41, 0.25, 0, 1),
        ("AB", 0.4, true, 0.5, 0.41, 0.25, 0, 1),
        ("AB", 0.6, true, 0.5, 0.41, 0.25, 0, 1),
        ("AB", 2.0, true, 0.5, 0.41, 0.25, 0, 1),
        // AC: flips between 0.7071 and 0.7072; the answer is the crossing
        // into B, not the destination.
        ("AC", -1.0, true, 1.0 / 3.0, 0.6, 0.426190, 0, 1),
        ("AC", 0.0, true, 1.0 / 3.0, 0.6, 0.426190, 0, 1),
        ("AC", 0.5, false, 0.0, 0.0, 0.0, 0, 0),
        ("AC", 0.65, false, 0.0, 0.0, 0.0, 0, 0),
        ("AC", 0.70, false, 0.0, 0.0, 0.0, 0, 0),
        ("AC", 0.7071, false, 0.0, 0.0, 0.0, 0, 0),
        ("AC", 0.7072, true, 1.0 / 3.0, 0.6, 0.426190, 0, 1),
        ("AC", 0.71, true, 1.0 / 3.0, 0.6, 0.426190, 0, 1),
        ("AC", 0.8, true, 1.0 / 3.0, 0.6, 0.426190, 0, 1),
        ("AC", 2.0, true, 1.0 / 3.0, 0.6, 0.426190, 0, 1),
        // CA: flips between 0.7280 and 0.7281 — a different threshold than
        // AC over the same portal chain, so the limit rides the pair
        // distance and nothing else.
        ("CA", -1.0, true, 2.0 / 3.0, 0.6, 0.5, 0, 1),
        ("CA", 0.0, true, 2.0 / 3.0, 0.6, 0.5, 0, 1),
        ("CA", 0.70, false, 0.0, 0.0, 0.0, 0, 0),
        ("CA", 0.7279, false, 0.0, 0.0, 0.0, 0, 0),
        ("CA", 0.7280, false, 0.0, 0.0, 0.0, 0, 0),
        ("CA", 0.7281, true, 2.0 / 3.0, 0.6, 0.5, 0, 1),
        ("CA", 0.73, true, 2.0 / 3.0, 0.6, 0.5, 0, 1),
        ("CA", 0.8, true, 2.0 / 3.0, 0.6, 0.5, 0, 1),
        ("CA", 2.0, true, 2.0 / 3.0, 0.6, 0.5, 0, 1),
        // BC: flips between 0.4300 and 0.4302; the answer is the
        // destination itself (the crossing candidate is waypoint-equal to
        // it, so findPath swaps dst in).
        ("BC", -1.0, true, 0.85, 0.41, 0.5, 0, 2),
        ("BC", 0.0, true, 0.85, 0.41, 0.5, 0, 2),
        ("BC", 0.35, false, 0.0, 0.0, 0.0, 0, 0),
        ("BC", 0.42, false, 0.0, 0.0, 0.0, 0, 0),
        ("BC", 0.43, false, 0.0, 0.0, 0.0, 0, 0),
        ("BC", 0.4302, true, 0.85, 0.41, 0.5, 0, 2),
        ("BC", 0.44, true, 0.85, 0.41, 0.5, 0, 2),
        ("BC", 2.0, true, 0.85, 0.41, 0.5, 0, 2),
        // BB: one set, two waypoints — only the in-set filter binds
        // (flip between 0.3140 and 0.3141, the crossing candidate).
        ("BB", -1.0, true, 0.5, 0.41, 0.75, 1, 1),
        ("BB", 0.0, true, 0.5, 0.41, 0.75, 1, 1),
        ("BB", 0.25, false, 0.0, 0.0, 0.0, 0, 0),
        ("BB", 0.30, false, 0.0, 0.0, 0.0, 0, 0),
        ("BB", 0.313, false, 0.0, 0.0, 0.0, 0, 0),
        ("BB", 0.314, false, 0.0, 0.0, 0.0, 0, 0),
        ("BB", 0.3141, true, 0.5, 0.41, 0.75, 1, 1),
        ("BB", 0.315, true, 0.5, 0.41, 0.75, 1, 1),
        ("BB", 0.32, true, 0.5, 0.41, 0.75, 1, 1),
        ("BB", 0.5, true, 0.5, 0.41, 0.75, 1, 1),
        ("BB", 2.0, true, 0.5, 0.41, 0.75, 1, 1),
        // AE: the destination just past the A|B portal — the set level
        // clears from 0.254 but the in-set crossing still trips the limit
        // (flip between 0.2656 and 0.2657).
        ("AE", -1.0, true, 0.4, 0.41, 0.36, 0, 1),
        ("AE", 0.0, true, 0.4, 0.41, 0.36, 0, 1),
        ("AE", 0.254, false, 0.0, 0.0, 0.0, 0, 0),
        ("AE", 0.26, false, 0.0, 0.0, 0.0, 0, 0),
        ("AE", 0.265, false, 0.0, 0.0, 0.0, 0, 0),
        ("AE", 0.2656, false, 0.0, 0.0, 0.0, 0, 0),
        ("AE", 0.2657, true, 0.4, 0.41, 0.36, 0, 1),
        ("AE", 0.267, true, 0.4, 0.41, 0.36, 0, 1),
        ("AE", 0.27, true, 0.4, 0.41, 0.36, 0, 1),
        ("AE", 0.4, true, 0.4, 0.41, 0.36, 0, 1),
        ("AE", 2.0, true, 0.4, 0.41, 0.36, 0, 1),
    ];

    let next_sets = [&a, &b, &c];
    for (pair, limit, ok, x, y, z, wp, set_index) in rows {
        let (src_set, src_wp, src, dst_set, dst_wp, dst) = endpoints(pair, &a, &b, &c);
        let src = loc(src_set, src_wp, src.x, src.y, src.z);
        let dst = loc(dst_set, dst_wp, dst.x, dst.y, dst.z);
        let mut next = NavLoc::new();
        let mut passed_activated_portal = false;
        let found = Navigator::default().find_path(
            &src,
            &dst,
            *limit,
            false,
            &mut next,
            &mut passed_activated_portal,
            CacheState::ReadWriteCache,
        );
        assert_eq!(
            found,
            *ok,
            "DOK/DPNONE {pair} {limit}: the original answers {}",
            if *ok { "OK" } else { "NONE" }
        );
        if *ok {
            assert!(
                point_near(next.point(), *x, *y, *z),
                "DOK {pair} {limit}: {:?}",
                next.point()
            );
            assert_eq!(next.waypoint(), *wp, "DOK {pair} {limit}");
            assert!(
                Rc::ptr_eq(next.p_set().unwrap(), next_sets[*set_index]),
                "DOK {pair} {limit}: the next set"
            );
            assert!(!passed_activated_portal, "DOK {pair} {limit}: act flag");
        } else {
            assert!(
                next.point().y == 0.0,
                "DPNONE {pair} {limit}: the untouched next stays invalid"
            );
        }
    }
}

/// The `DCACHEOK`/`DCACHENONE` lines: a changed limit on one `Navigator`.
/// Every set path in this world crosses a shell boundary (all chunks are
/// inside, so each cross-set hop sets the flag), and the reference
/// bypasses the set cache whenever it did — so each call re-searches and
/// a limit tighter than the pair distance keeps failing even on the
/// navigator that just answered unlimited.
#[test]
fn a_changed_limit_on_the_same_navigator_always_researches() {
    let (sets, _) = wired_world();
    let (a, c) = (sets[0].clone(), sets[2].clone());

    let src = loc(&a, 0, 0.15, 0.41, 0.4);
    let dst = loc(&c, 0, 0.85, 0.41, 0.5);
    let navigator = Navigator::default();

    let answer = |limit: f32| {
        let mut next = NavLoc::new();
        let mut passed_activated_portal = false;
        let found = navigator.find_path(
            &src,
            &dst,
            limit,
            false,
            &mut next,
            &mut passed_activated_portal,
            CacheState::ReadWriteCache,
        );
        (found, next)
    };

    // DCACHEOK 0 -1.0000: the unlimited search answers the B crossing.
    let (found, next) = answer(-1.0);
    assert!(found);
    assert!(point_near(next.point(), 1.0 / 3.0, 0.6, 0.426190));

    // DCACHENONE 1 0.0500 — the cached set path is bypassed, so the tight
    // limit re-searches and fails.
    let (found, _) = answer(0.05);
    assert!(!found, "DCACHENONE 1");

    // DCACHEOK 2 0.0000: zero disables the check, like -1.
    let (found, next) = answer(0.0);
    assert!(found);
    assert!(point_near(next.point(), 1.0 / 3.0, 0.6, 0.426190));

    // DCACHENONE 3 0.3000.
    let (found, _) = answer(0.3);
    assert!(!found, "DCACHENONE 3");
}
