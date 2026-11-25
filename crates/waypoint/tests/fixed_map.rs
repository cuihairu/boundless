//! BW-10-07-01 — the fixed test map (a slope, a hole, a raised obstacle,
//! and two adjacent chunks), replayed against the original engine's own
//! output.
//!
//! The fixture (`docs/review/fixtures/waypoint/gen_fixed_map.cpp`) links
//! the reference el7 libraries and assembles a real two-chunk world:
//! chunk A (set 0) with the hole at x in [0.2, 0.3] and the slope
//! A1 (band 0.6..1.0) bridging its two ground strips, chunk B (set 1)
//! with the ring around the raised obstacle B3 (band 1.1..1.5, the 0.1
//! band gap plus all-vista edges leave it isolated), and the original
//! `ChunkWaypointSet::bind()` wiring a permissive portal pair on
//! x = 1/2. Its truth log (`gen_fixed_map.log`) is what these tests pin:
//! - the decode (HDR/SET/WP/EDGE),
//! - the bind output (PORTAL/CONNTO/EDGELABEL) re-created through the
//!   port's `connect()`,
//! - the neighbour iterator (ITER — born dead, like 04-05),
//! - where the reference resolver drops each probe point (NAVLOC — the
//!   hole centre and the obstacle ground are find-closest ties),
//! - the chunk-level A* (XPATH — the hole detour rides the slope, the
//!   reverse crossing dies on the crossed-portal state),
//! - the set-level A* (SETSEARCH/SETHOP — the permissive pair answers
//!   regardless of the block flag),
//! - the original navigator's findPath (FP).
//!
//! One registered boundary shows up on this map: the reference's
//! `saveWayPath` runs `smoothPath` (string-pulling) before
//! `removeDuplicates`, and the funnel rebuilds each state through the
//! guess ctor, so the reference's FPOK points carry a +0.01 y lift
//! (`1.010000` where the raw A* state says `1.000000`). The port defers
//! that face, so the FPOK probes here pin the unlifted y while naming the
//! lifted reference value in the assertion message.

use foundation::math::Vector3;
use std::rc::Rc;
use waypoint::{
    AStar, CacheState, ChunkWPSetState, ChunkWaypointState, NavLoc, Navigator, NavigatorFindResult,
    Portal, WaypointNeighbourIterator, find_by_girth, parse_world_navmesh,
};

const FIXTURE_DIR: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../docs/review/fixtures/waypoint"
);

fn fixture_bytes(name: &str) -> Vec<u8> {
    std::fs::read(format!("{FIXTURE_DIR}/{name}")).expect("fixture")
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

/// The world the reference `bind()` produced: two sets on chunks A/B, the
/// two permissive portals (A east -> B, B west -> A), and every `connect()`
/// the log's CONNTO/EDGELABEL lines report. Returns the sets and the
/// portals so tests can compare portal identities.
fn wired_world() -> (Vec<Rc<waypoint::ChunkWaypointSetData>>, [Rc<Portal>; 2]) {
    let sets: Vec<Rc<waypoint::ChunkWaypointSetData>> =
        parse_world_navmesh(&fixture_bytes("world_navmesh_fixed_map.bin"))
            .expect("the fixture decodes")
            .into_iter()
            .map(Rc::new)
            .collect();
    assert_eq!(sets.len(), 2, "the log's two SET lines");

    for (chunk_id, set) in sets.iter().enumerate() {
        set.set_chunk_identity(chunk_id);
    }
    let [a, b] = [sets[0].clone(), sets[1].clone()];

    let permissive0 = Rc::new(Portal {
        permissive: true,
        has_chunk_item: false,
    });
    let permissive1 = Rc::new(Portal {
        permissive: true,
        has_chunk_item: false,
    });

    // CONNTO/EDGELABEL: A --p0--> B over edges 6 (A0e east -> B wp0) and
    // 10 (A1 east -> B wp1); B --p1--> A over edges 0 (B0 west -> A wp1)
    // and 12 (B2w west -> A wp1). B1's west edge (6) got NO label: the
    // reference bind's intersection walk skipped it — the pinned
    // asymmetry of the partial portal rect (z in [0.1, 0.9]).
    a.connect(
        b.clone(),
        permissive0.clone(),
        6,
        0,
        Vector3::new(0.5, 0.5, 0.25),
    );
    a.connect(
        b.clone(),
        permissive0.clone(),
        10,
        1,
        Vector3::new(0.5, 0.9, 0.75),
    );
    b.connect(
        a.clone(),
        permissive1.clone(),
        0,
        1,
        Vector3::new(0.5, 0.5, 0.2),
    );
    b.connect(
        a.clone(),
        permissive1.clone(),
        12,
        1,
        Vector3::new(0.5, 0.5, 0.5),
    );

    (sets, [permissive0, permissive1])
}

#[test]
fn the_fixture_decodes_two_sets_as_logged() {
    let sets = parse_world_navmesh(&fixture_bytes("world_navmesh_fixed_map.bin"))
        .expect("the fixture decodes");

    // HDR/SET: girth 0.6, bases 1/4, waypoint counts 3/5.
    assert_eq!(sets.len(), 2);
    assert_eq!(sets[0].base_index(), 1);
    assert_eq!(sets[1].base_index(), 4);
    assert_eq!(sets[0].waypoints().len(), 3);
    assert_eq!(sets[1].waypoints().len(), 5);
    for set in &sets {
        assert!(near(set.girth(), 0.6));
    }

    // WP: A0w/A0e on the ground band, A1 the slope (0.6..1.0 — touching
    // bands stay adjacent), B3 the obstacle top (1.1..1.5 — the 0.1 gap
    // breaks height adjacency).
    let a = &sets[0];
    let (min_h, max_h) = (a.waypoints()[0].min_height, a.waypoints()[0].max_height);
    assert!(near(min_h, 0.2) && near(max_h, 0.6));
    assert!(near(a.waypoints()[1].min_height, 0.2));
    assert!(near(a.waypoints()[1].max_height, 0.6));
    assert!(near(a.waypoints()[2].min_height, 0.6));
    assert!(near(a.waypoints()[2].max_height, 1.0));
    assert!(near(a.waypoints()[0].centre.x, 0.1));
    assert!(near(a.waypoints()[0].centre.y, 0.25));
    assert!(near(a.waypoints()[1].centre.x, 0.4));
    assert!(near(a.waypoints()[2].centre.x, 0.25));
    assert!(near(a.waypoints()[2].centre.y, 0.75));

    let b = &sets[1];
    for wp in 0..4 {
        assert!(near(b.waypoints()[wp].min_height, 0.2));
        assert!(near(b.waypoints()[wp].max_height, 0.6));
    }
    assert!(near(b.waypoints()[4].min_height, 1.1));
    assert!(near(b.waypoints()[4].max_height, 1.5));
    assert!(near(b.waypoints()[2].centre.x, 0.575));
    assert!(near(b.waypoints()[3].centre.x, 0.925));
    assert!(near(b.waypoints()[4].centre.x, 0.75));
    assert!(near(b.waypoints()[4].centre.y, 0.5));

    // EDGE: the slope links (A0w/A0e north -> A1 wp2, A1's split south
    // edge 1/vista/0), the ring links (B0 north 2/vista/3, B1 south
    // 3/vista/2), the obstacle's all-vista face, and the boundary/vista
    // encodings (FFFF boundary, FFFE vista = the ~-1 top-bit form).
    let a_edges = |wp: usize| -> Vec<u16> {
        a.waypoint_edges(wp)
            .iter()
            .map(|edge| edge.neighbour)
            .collect()
    };
    assert_eq!(
        a_edges(0),
        [u16::MAX, 2, 0xFFFE, u16::MAX],
        "A0w: boundary / A1 / hole vista / boundary"
    );
    assert_eq!(
        a_edges(1),
        [0xFFFE, 2, u16::MAX, u16::MAX],
        "A0e: hole vista / A1 / portal face / boundary"
    );
    assert_eq!(
        a_edges(2),
        [u16::MAX, u16::MAX, u16::MAX, 1, 0xFFFE, 0],
        "A1: split south edge A0e / hole / A0w"
    );

    let b_edges = |wp: usize| -> Vec<u16> {
        b.waypoint_edges(wp)
            .iter()
            .map(|edge| edge.neighbour)
            .collect()
    };
    assert_eq!(
        b_edges(0),
        [u16::MAX, 2, 0xFFFE, 3, u16::MAX, u16::MAX],
        "B0: north split B2w / obstacle vista / B2e"
    );
    assert_eq!(
        b_edges(1),
        [u16::MAX, u16::MAX, u16::MAX, 3, 0xFFFE, 2],
        "B1: south split B2e / obstacle vista / B2w"
    );
    assert_eq!(
        b_edges(2),
        [u16::MAX, 1, 0xFFFE, 0],
        "B2w: north B1 / obstacle vista / south B0"
    );
    assert_eq!(
        b_edges(3),
        [0xFFFE, 1, u16::MAX, 0],
        "B2e: west obstacle vista / north B1 / south B0"
    );
    assert_eq!(
        b_edges(4),
        [0xFFFE, 0xFFFE, 0xFFFE, 0xFFFE],
        "B3: every edge vista — graph-isolated"
    );
}

#[test]
fn the_bind_wiring_restores_connections_and_labels() {
    let (sets, portals) = wired_world();
    let [a, b] = [&sets[0], &sets[1]];
    let [p0, p1] = &portals;

    // CONNTO 0 0 1 0 / CONNTO 1 0 0 1: one connection per set, both
    // through the permissive portal pair.
    let a_conns = a.connections();
    assert_eq!(a_conns.len(), 1);
    assert!(Rc::ptr_eq(&a_conns[0].0, b));
    assert!(Rc::ptr_eq(&a_conns[0].1, p0));

    let b_conns = b.connections();
    assert_eq!(b_conns.len(), 1);
    assert!(Rc::ptr_eq(&b_conns[0].0, a));
    assert!(Rc::ptr_eq(&b_conns[0].1, p1));

    // EDGELABEL: A's two east-facing edges label into B (edge 6 -> B wp0,
    // edge 10 -> B wp1); B's edges 0 and 12 both label into A wp1. The
    // entry points are the bind probe points, not portal projections.
    let label = |set: &Rc<waypoint::ChunkWaypointSetData>, edge: u16| {
        let label = set
            .edge_label(edge)
            .unwrap_or_else(|| panic!("edge {edge}"));
        (label.set.clone(), label.waypoint, label.entry_point)
    };

    let (set, wp, entry) = label(a, 6);
    assert!(Rc::ptr_eq(&set, b));
    assert_eq!(wp, 0);
    assert!(point_near(entry, 0.5, 0.5, 0.25));

    let (set, wp, entry) = label(a, 10);
    assert!(Rc::ptr_eq(&set, b));
    assert_eq!(wp, 1);
    assert!(point_near(entry, 0.5, 0.9, 0.75));

    let (set, wp, entry) = label(b, 0);
    assert!(Rc::ptr_eq(&set, a));
    assert_eq!(wp, 1);
    assert!(point_near(entry, 0.5, 0.5, 0.2));

    let (set, wp, entry) = label(b, 12);
    assert!(Rc::ptr_eq(&set, a));
    assert_eq!(wp, 1);
    assert!(point_near(entry, 0.5, 0.5, 0.5));

    // The pinned asymmetry: B1's west edge (edge 6) intersects the portal
    // rect z in [0.1, 0.9] at z in [0.6, 0.9], yet the reference bind
    // never labeled it — the intersection walk stopped at B2w's edge.
    assert!(b.edge_label(6).is_none(), "B1's west edge stays unlabeled");

    // The backlinks connect added on first sight (one per neighbour).
    assert_eq!(a.backlinks().len(), 1);
    assert_eq!(b.backlinks().len(), 1);
}

#[test]
fn the_neighbour_iterator_matches_the_reference_walk() {
    let (sets, _) = wired_world();

    // ITER 0 0..2 / 1 0..4 all answer (count 0): the reference iterator
    // seeds its cursor past the loop guard, so a fresh walk is already
    // ended.
    for (set_index, waypoint) in [
        (0usize, 0i32),
        (0, 1),
        (0, 2),
        (1, 0),
        (1, 1),
        (1, 2),
        (1, 3),
        (1, 4),
    ] {
        let iterator = WaypointNeighbourIterator::new(&sets[set_index], waypoint);
        assert!(
            iterator.ended(),
            "ITER {set_index} {waypoint}: the reference walk is empty"
        );
        assert!(iterator.p_neighbour_set().is_none());
        assert_eq!(iterator.neighbour_waypoint_index(), -1);
    }
}

/// The NAVLOC probes: where the reference resolver drops each point. The
/// exact contains resolve by band (the obstacle top lands on B3); the
/// hole centre and the obstacle ground are find-closest ties the
/// first-iterated waypoint wins.
#[test]
fn the_reference_resolver_drops_the_probe_points_as_logged() {
    let sets = parse_world_navmesh(&fixture_bytes("world_navmesh_fixed_map.bin"))
        .expect("the fixture decodes")
        .into_iter()
        .map(Rc::new)
        .collect::<Vec<_>>();
    let [a, b] = [sets[0].clone(), sets[1].clone()];

    // NAVLOC NA0W 0 0 (0.1,0.41,0.25): plain ground, exact.
    let res = find_by_girth(
        std::slice::from_ref(&a),
        Vector3::new(0.1, 0.4, 0.25),
        0.6,
        false,
    );
    assert_eq!(res.waypoint(), 0);
    assert!(res.exact_match());

    // NAVLOC NSLOPE 2 0 (0.25,0.81,0.75): the slope resolves by its own
    // band 0.6..1.0.
    let res = find_by_girth(
        std::slice::from_ref(&a),
        Vector3::new(0.25, 0.8, 0.75),
        0.6,
        false,
    );
    assert_eq!(res.waypoint(), 2);
    assert!(res.exact_match());

    // NAVLOC NHOLE 0 0 (0.25,0.41,0.25): the hole centre misses (the gap
    // x in [0.2, 0.3] has no waypoint) and the closest fallback ties
    // A0w/A0e at 0.05 — the first-iterated A0w wins.
    let res = find_by_girth(
        std::slice::from_ref(&a),
        Vector3::new(0.25, 0.4, 0.25),
        0.6,
        false,
    );
    assert_eq!(res.waypoint(), 0, "the hole centre falls back to A0w");
    assert!(!res.exact_match());

    // NAVLOC NB0 0 1 (0.75,0.41,0.2).
    let res = find_by_girth(
        std::slice::from_ref(&b),
        Vector3::new(0.75, 0.4, 0.2),
        0.6,
        false,
    );
    assert_eq!(res.waypoint(), 0);
    assert!(res.exact_match());

    // NAVLOC NOBSTTOP 4 1 (0.75,1.31,0.5): the obstacle top resolves by
    // its own band 1.1..1.5 — the top is locatable but unreachable.
    let res = find_by_girth(
        std::slice::from_ref(&b),
        Vector3::new(0.75, 1.3, 0.5),
        0.6,
        false,
    );
    assert_eq!(res.waypoint(), 4);
    assert!(res.exact_match());

    // NAVLOC NOBSTGROUND 0 1 (0.75,0.41,0.5): the ground point under the
    // obstacle misses B3's band (0.4 is 0.7 under 1.1) and the closest
    // fallback ties B0/B2w/B2e at 0.1 — the first-iterated B0 wins.
    let res = find_by_girth(
        std::slice::from_ref(&b),
        Vector3::new(0.75, 0.4, 0.5),
        0.6,
        false,
    );
    assert_eq!(res.waypoint(), 0, "the obstacle ground falls back to B0");
    assert!(!res.exact_match());

    // The located points carry the reference's +0.01 height bump
    // (NAVLOC y values 0.41/0.81/1.31). The find-based locate replays it
    // through from_guess with a non-containing seed.
    let resolve = |set: &Rc<waypoint::ChunkWaypointSetData>, x: f32, y: f32, z: f32| -> NavLoc {
        let seed = NavLoc::from_parts(set.clone(), 0, Vector3::new(-1.0, -1.0, -1.0), 1.0);
        NavLoc::from_guess(&seed, set.clone(), 1.0, Vector3::new(x, y, z))
    };
    let located = resolve(&a, 0.1, 0.4, 0.25);
    assert_eq!(located.waypoint(), 0);
    assert!(point_near(located.point(), 0.1, 0.41, 0.25), "NA0W");
    let located = resolve(&a, 0.25, 0.8, 0.75);
    assert_eq!(located.waypoint(), 2);
    assert!(point_near(located.point(), 0.25, 0.81, 0.75), "NSLOPE");
    let located = resolve(&b, 0.75, 1.3, 0.5);
    assert_eq!(located.waypoint(), 4);
    assert!(point_near(located.point(), 0.75, 1.31, 0.5), "NOBSTTOP");
}

/// The chunk-level XPATH probes, straight off the log. The CROSS state
/// chain shows the hole detour: A0w can only reach A0e over the slope,
/// and the crossing into B lands on the portal face. The reverse crossing
/// dies on the crossed-portal state (the 04-05 quirk), and the obstacle
/// top is unreachable.
#[test]
fn the_chunk_level_paths_cross_hole_and_obstacle_as_logged() {
    let (sets, _) = wired_world();
    let [a, b] = [&sets[0], &sets[1]];

    let run = |src: (&Rc<waypoint::ChunkWaypointSetData>, i32, f32, f32, f32),
               dst: (&Rc<waypoint::ChunkWaypointSetData>, i32, f32, f32, f32)| {
        let src = ChunkWaypointState::from_navloc(&loc(src.0, src.1, src.2, src.3, src.4));
        let dst = ChunkWaypointState::from_navloc(&loc(dst.0, dst.1, dst.2, dst.3, dst.4));
        let mut astar = AStar::default();
        let found = astar.search(&src, &dst, -1.0);
        (found, astar.path().to_vec())
    };

    // XPATH CROSS: four states — A0w, the slope corner (0.2,1.0,0.5) on
    // A1, A0e at (0.3,0.6,0.5), then the B0 crossing at (0.5,0.6,0.366667).
    let (found, path) = run((a, 0, 0.1, 0.41, 0.25), (b, 0, 0.75, 0.41, 0.2));
    assert!(found, "XPATH CROSS");
    assert_eq!(path.len(), 4);
    assert!(point_near(path[0].nav_loc().point(), 0.1, 0.41, 0.25));
    assert_eq!(path[0].nav_loc().waypoint(), 0);
    assert!(Rc::ptr_eq(path[0].nav_loc().p_set().unwrap(), a));
    assert!(
        point_near(path[1].nav_loc().point(), 0.2, 1.0, 0.5),
        "{:?}",
        path[1].nav_loc().point()
    );
    assert_eq!(path[1].nav_loc().waypoint(), 2, "the slope hop");
    assert!(point_near(path[2].nav_loc().point(), 0.3, 0.6, 0.5));
    assert_eq!(path[2].nav_loc().waypoint(), 1);
    assert!(
        point_near(path[3].nav_loc().point(), 0.5, 0.6, 0.366667),
        "{:?}",
        path[3].nav_loc().point()
    );
    assert_eq!(path[3].nav_loc().waypoint(), 0);
    assert!(Rc::ptr_eq(path[3].nav_loc().p_set().unwrap(), b));

    // XPATHNONE CROSSBA: the crossed-portal state refuses further
    // expansion — B0's label lands on A0e but the goal is A0w, one slope
    // and one hole crossing deeper.
    let (found, _) = run((b, 0, 0.75, 0.41, 0.2), (a, 0, 0.1, 0.41, 0.25));
    assert!(!found, "XPATHNONE CROSSBA");

    // XPATH HOLE: the acceptance route — A0w -> A1 (the slope) -> A0e,
    // around the hole through the touching height bands.
    let (found, path) = run((a, 0, 0.1, 0.41, 0.25), (a, 1, 0.4, 0.41, 0.25));
    assert!(found, "XPATH HOLE");
    assert_eq!(path.len(), 3);
    assert_eq!(path[1].nav_loc().waypoint(), 2, "the detour rides A1");
    assert!(point_near(path[1].nav_loc().point(), 0.2, 1.0, 0.5));
    assert_eq!(path[2].nav_loc().waypoint(), 1);
    assert!(point_near(path[2].nav_loc().point(), 0.3, 0.6, 0.5));

    // XPATH OBST: around the obstacle through the south strip — B2w, B0,
    // B2e.
    let (found, path) = run((b, 2, 0.575, 0.41, 0.5), (b, 3, 0.925, 0.41, 0.5));
    assert!(found, "XPATH OBST");
    assert_eq!(path.len(), 3);
    assert_eq!(path[0].nav_loc().waypoint(), 2);
    assert!(point_near(path[0].nav_loc().point(), 0.575, 0.41, 0.5));
    assert_eq!(path[1].nav_loc().waypoint(), 0, "the south-strip rung");
    assert!(point_near(path[1].nav_loc().point(), 0.65, 0.6, 0.4));
    assert_eq!(path[2].nav_loc().waypoint(), 3);
    assert!(point_near(path[2].nav_loc().point(), 0.85, 0.6, 0.4));

    // XPATHNONE WALLTOP: the obstacle top is graph-isolated (all-vista
    // edges over a 0.1 band gap).
    let (found, _) = run((b, 2, 0.575, 0.41, 0.5), (b, 4, 0.75, 1.31, 0.5));
    assert!(!found, "XPATHNONE WALLTOP");
}

/// The SETSEARCH probes: the permissive portal pair answers regardless of
/// the block flag, both directions, one hop each.
#[test]
fn the_set_level_search_crosses_the_permissive_pair_both_ways() {
    let (sets, portals) = wired_world();
    let [a, b] = [&sets[0], &sets[1]];
    let [p0, p1] = &portals;

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

    // SETHOP AB0: two states, shell flag on the crossing, portal 0.
    let (found, path) = run((a, 0, 0.1, 0.41, 0.25), (b, 0, 0.75, 0.41, 0.2), false);
    assert!(found, "SETSEARCH AB0");
    assert_eq!(path.len(), 2);
    assert!(Rc::ptr_eq(path[0].p_set().unwrap(), a));
    assert!(point_near(path[0].position(), 0.1, 0.41, 0.25));
    assert!(Rc::ptr_eq(path[1].p_set().unwrap(), b));
    assert!(point_near(path[1].position(), 0.75, 0.41, 0.2));
    assert!(!path[0].passed_shell_boundary());
    assert!(path[1].passed_shell_boundary());
    assert!(!path[1].passed_activated_portal());
    let hop = a.connection_portal(path[1].p_set().unwrap()).unwrap();
    assert!(Rc::ptr_eq(&hop, p0), "AB0 portal is 0");

    // SETHOP AB1: blockNonPermissive keeps the permissive pair usable.
    let (found, path) = run((a, 0, 0.1, 0.41, 0.25), (b, 0, 0.75, 0.41, 0.2), true);
    assert!(found, "SETSEARCH AB1: the pair is permissive");
    assert_eq!(path.len(), 2);
    let hop = a.connection_portal(path[1].p_set().unwrap()).unwrap();
    assert!(Rc::ptr_eq(&hop, p0));

    // SETHOP BA0: the mirror crossing through portal 1.
    let (found, path) = run((b, 0, 0.75, 0.41, 0.2), (a, 0, 0.1, 0.41, 0.25), false);
    assert!(found, "SETSEARCH BA0");
    assert_eq!(path.len(), 2);
    assert!(point_near(path[1].position(), 0.1, 0.41, 0.25));
    assert!(path[1].passed_shell_boundary());
    let hop = b.connection_portal(path[1].p_set().unwrap()).unwrap();
    assert!(Rc::ptr_eq(&hop, p1), "BA0 portal is 1");
}

/// The findPath probes (`FPBEGIN`/`FPOK`/`FPNONE`/`FPEND`): the original
/// navigator's answer over the fixed map. The `next` is the first hop of
/// the way path — over the slope for the hole detour, into the south
/// strip around the obstacle — and the obstacle top stays unreachable.
/// The reference's FPOK y carries the funnel's +0.01 lift; the port's
/// unsmoothed path does not (see the module header).
#[test]
fn the_navigator_answers_the_reference_findpath_probes() {
    let (sets, _) = wired_world();
    let [a, b] = [&sets[0], &sets[1]];

    let answer = |navigator: &Navigator, src: &NavLoc, dst: &NavLoc| -> (bool, NavLoc, bool) {
        let mut next = NavLoc::new();
        let mut passed_activated_portal = false;
        let found = navigator.find_path(
            src,
            dst,
            1_000_000.0,
            false,
            &mut next,
            &mut passed_activated_portal,
            CacheState::ReadWriteCache,
        );
        (found, next, passed_activated_portal)
    };

    // FPOK CROSS: the reference line is `0.200000 1.010000 0.500000 2 0 0` —
    // the first hop is the slope corner, the hole forcing the climb even on
    // the way to B.  `saveWayPath` runs `smoothPath` before
    // `removeDuplicates` (navigator_cache.cpp:38), and the funnel rebuilds
    // every state through the guess ctor `NavLoc( navLoc, newPoint )`
    // (navloc.cpp:60, `point.y += 0.01`), so the FPOK y carries that lift.
    let navigator = Navigator::default();
    let src = loc(a, 0, 0.1, 0.41, 0.25);
    let dst = loc(b, 0, 0.75, 0.41, 0.2);
    let (found, next, act) = answer(&navigator, &src, &dst);
    assert!(found, "FPOK CROSS");
    assert!(
        point_near(next.point(), 0.2, 1.01, 0.5),
        "{:?}",
        next.point()
    );
    assert_eq!(next.waypoint(), 2);
    assert!(Rc::ptr_eq(next.p_set().unwrap(), a));
    assert!(!act, "the reference's act flag stays 0");

    // The smoothed cached path: the funnel's corner points with the maxY
    // chain lifting each onto the tallest state they skip over, the last
    // entry the crossing state into B's south strip.
    let cached = navigator.get_cached_waypoint_path();
    assert_eq!(cached.len(), 3, "the saved path keeps all four states");
    assert!(point_near(cached[0], 0.2, 1.01, 0.5), "{cached:?}");
    assert!(point_near(cached[1], 0.3, 1.01, 0.5), "{cached:?}");
    assert!(point_near(cached[2], 0.5, 0.61, 0.366667), "{cached:?}");

    // FPOK CROSSR: the exact repeat answers the same off the cached path.
    let (found, next, _) = answer(&navigator, &src, &dst);
    assert!(found, "FPOK CROSSR");
    assert!(point_near(next.point(), 0.2, 1.01, 0.5));
    assert_eq!(next.waypoint(), 2);

    // FPOK HOLE (0.2,1.01,0.5) wp2: the slope detour's first hop.
    let hole_dst = loc(a, 1, 0.4, 0.41, 0.25);
    let (found, next, _) = answer(&navigator, &src, &hole_dst);
    assert!(found, "FPOK HOLE");
    assert!(point_near(next.point(), 0.2, 1.01, 0.5));
    assert_eq!(next.waypoint(), 2);
    assert!(Rc::ptr_eq(next.p_set().unwrap(), a));

    // FPOK HOLER (0.3,1.01,0.5) wp2: the mirror climb from A0e.
    let holer_src = loc(a, 1, 0.4, 0.41, 0.25);
    let holer_dst = loc(a, 0, 0.1, 0.41, 0.25);
    let (found, next, _) = answer(&navigator, &holer_src, &holer_dst);
    assert!(found, "FPOK HOLER");
    assert!(point_near(next.point(), 0.3, 1.01, 0.5));
    assert_eq!(next.waypoint(), 2);

    // FPOK OBST (0.65,0.61,0.4) wp0 set1: around the obstacle via the
    // south strip.
    let obst_src = loc(b, 2, 0.575, 0.41, 0.5);
    let obst_dst = loc(b, 3, 0.925, 0.41, 0.5);
    let (found, next, _) = answer(&navigator, &obst_src, &obst_dst);
    assert!(found, "FPOK OBST");
    assert!(point_near(next.point(), 0.65, 0.61, 0.4));
    assert_eq!(next.waypoint(), 0);
    assert!(Rc::ptr_eq(next.p_set().unwrap(), b));

    // FPNONE WALLTOP: the obstacle top stays unreachable.
    let wall_dst = loc(b, 4, 0.75, 1.31, 0.5);
    let (found, next, _) = answer(&navigator, &obst_src, &wall_dst);
    assert!(!found, "FPNONE WALLTOP");
    assert!(next.point().y == 0.0, "the untouched next stays invalid");
}

/// The findFullPath probes. The reference's own answer over this fixture
/// is `FFPNONE` for every probe — its space constructor resolves both
/// endpoints to null (`FPLOC ... 0 0`), because the fixture's chunks are
/// mounted on their own navigators and never registered with the space,
/// so `findChunkFromPointExact` never finds them and the `src.valid() &&
/// dst.valid()` guard closes before the search runs. That is a property
/// of the fixture's world wiring, not of the waypoint graph: the port's
/// `find_full_path` takes the same navlocs the space constructor would
/// hand it, so this pins the algorithmic face against the chains the log
/// already fixed (XPATH for the hops, FPOK for the last point).
#[test]
fn the_navigator_answers_the_reference_findfullpath_probes() {
    let (sets, _) = wired_world();
    let [a, b] = [&sets[0], &sets[1]];

    let run = |navigator: &Navigator, src: &NavLoc, dst: &NavLoc| {
        let mut path = Vec::new();
        let found = navigator.find_full_path(src, dst, 1_000_000.0, false, &mut path);
        (found, path)
    };

    // FFP HOLE: the detour's three points — the slope corner, A0e's
    // centre — then the destination. The funnel's maxY chain lifts both
    // corners onto the slope (the tallest state each skips), the guess
    // ctor adding its +0.01.
    let navigator = Navigator::default();
    let src = loc(a, 0, 0.1, 0.41, 0.25);
    let dst = loc(a, 1, 0.4, 0.41, 0.25);
    let (found, path) = run(&navigator, &src, &dst);
    assert!(found, "FFP HOLE");
    assert_eq!(
        path,
        vec![
            Vector3::new(0.2, 1.01, 0.5),
            Vector3::new(0.3, 1.01, 0.5),
            Vector3::new(0.4, 0.6, 0.25),
        ],
        "the funnel-shaped detour plus the destination — the pushed \
         destination is the clipped point (clip's makeMaxHeight tail \
         lifts y to the waypoint's max height, the 06-01 quirk)"
    );

    // FFP CROSS: the same detour into B's south strip, then the
    // destination point (the goal check lands on the crossing state,
    // so the crossing point precedes the destination).
    let dst = loc(b, 0, 0.75, 0.41, 0.2);
    let (found, path) = run(&navigator, &src, &dst);
    assert!(found, "FFP CROSS");
    assert_eq!(
        path,
        vec![
            Vector3::new(0.2, 1.01, 0.5),
            Vector3::new(0.3, 1.01, 0.5),
            Vector3::new(0.5, 0.61, 0.36666667),
            Vector3::new(0.75, 0.6, 0.2),
        ]
    );

    // FFP OBST: the south-strip rung and B2e, then the destination.
    let obst_src = loc(b, 2, 0.575, 0.41, 0.5);
    let obst_dst = loc(b, 3, 0.925, 0.41, 0.5);
    let (found, path) = run(&navigator, &obst_src, &obst_dst);
    assert!(found, "FFP OBST");
    assert_eq!(
        path,
        vec![
            Vector3::new(0.65, 0.61, 0.6),
            Vector3::new(0.85, 0.61, 0.6),
            Vector3::new(0.925, 0.6, 0.5),
        ]
    );

    // FFPNONE WALLTOP: the obstacle top stays unreachable, and the
    // failed search leaves the path empty.
    let wall_dst = loc(b, 4, 0.75, 1.31, 0.5);
    let (found, path) = run(&navigator, &obst_src, &wall_dst);
    assert!(!found, "FFPNONE WALLTOP");
    assert!(path.is_empty(), "the failed search clears the path");
}

/// `find_by_girth`'s set-agnostic form: the two-chunk world answered as
/// one collection picks the same waypoints the chunk-scoped probes did —
/// the shape 07-03 (server load) consumes.
#[test]
fn the_whole_world_resolves_through_one_girth_search() {
    let sets = parse_world_navmesh(&fixture_bytes("world_navmesh_fixed_map.bin"))
        .expect("the fixture decodes")
        .into_iter()
        .map(Rc::new)
        .collect::<Vec<_>>();

    let world: Vec<Rc<waypoint::ChunkWaypointSetData>> = sets.to_vec();

    // A point in A resolves into set 0.
    let res = find_by_girth(&world, Vector3::new(0.1, 0.4, 0.25), 0.6, false);
    assert!(Rc::ptr_eq(res.p_set().unwrap(), &sets[0]));
    assert_eq!(res.waypoint(), 0);

    // A point in B resolves into set 1.
    let res = find_by_girth(&world, Vector3::new(0.75, 0.4, 0.2), 0.6, false);
    assert!(Rc::ptr_eq(res.p_set().unwrap(), &sets[1]));
    assert_eq!(res.waypoint(), 0);

    // The obstacle top still lands on B3 over the whole world.
    let res = find_by_girth(&world, Vector3::new(0.75, 1.3, 0.5), 0.6, false);
    assert!(Rc::ptr_eq(res.p_set().unwrap(), &sets[1]));
    assert_eq!(res.waypoint(), 4);

    // A girth with no sets finds nothing.
    let res: NavigatorFindResult = find_by_girth(&world, Vector3::new(0.1, 0.4, 0.25), 1.0, false);
    assert!(res.p_set().is_none());
    assert_eq!(res.waypoint(), -1);
}
