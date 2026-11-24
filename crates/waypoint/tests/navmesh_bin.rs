//! BW-10-04-01/02 integration: the original-engine navmesh fixture
//! (`docs/review/fixtures/waypoint/world_navmesh.bin`) must decode through
//! the port to every value in the printed ground truth (`gen_waypoints.log`,
//! dumped by the unmodified BigWorld `IWaypointView::asBinary` writer and
//! `ChunkWaypointSetData::readWaypointSet` reader faces):
//!
//! - the blob is the original writer's version 0 multi-set record (260
//!   bytes), exactly what `navmeshFactory` reads;
//! - set 0 (girth 0.6) chains three polygons A-B-C with shared corners
//!   (vertex dedup), one chunk-boundary edge (65535) and three vista
//!   edges; set 1 (girth 1.0) holds a lone quad;
//! - the polygons run clockwise in the x-z plane, the engine's own
//!   convention — the reference `containsProjection` rejects interior
//!   points of counterclockwise polygons;
//! - the truth lines come from the reference objects themselves
//!   (HDR/SET/WP/EDGE/ADJ/CHUNKBOUND/VISTA/CONTAINS/CONTAINSPROJ/FIND/
//!   FINDIGN probes).

use foundation::math::{Vector2, Vector3};
use waypoint::{ChunkWaypointVertexProvider, parse_world_navmesh};

const FIXTURE_DIR: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../docs/review/fixtures/waypoint"
);

fn fixture_bytes(name: &str) -> Vec<u8> {
    std::fs::read(format!("{FIXTURE_DIR}/{name}")).expect("fixture")
}

fn truth_lines() -> Vec<String> {
    std::fs::read_to_string(format!("{FIXTURE_DIR}/gen_waypoints.log"))
        .expect("truth log")
        .lines()
        .map(str::to_owned)
        .collect()
}

/// The `WP` truth lines: wp id, min/max height, edge count and centre.
#[derive(Clone, Debug, PartialEq)]
struct WpLine {
    id: usize,
    minh: f32,
    maxh: f32,
    edges: usize,
    centre: (f32, f32),
}

/// The `EDGE` truth lines: vertex index, vertex coordinates and the raw
/// neighbour encoding (the wp/edge position is implied by the record
/// order).
#[derive(Clone, Debug)]
struct EdgeLine {
    vertex_index: u16,
    x: f32,
    y: f32,
    neighbour: u16,
}

/// The `CONTAINS` truth lines: the set slot (0 for set 0, 1 for set 1),
/// the set-local waypoint and the probe point with the answer.
#[derive(Clone, Debug)]
struct ContainsLine {
    set: usize,
    wp: usize,
    point: Vector3,
    answer: bool,
}

/// The `CONTAINSPROJ` truth lines: the set slot, the set-local waypoint
/// and the probe x-z point (the height is irrelevant to the projection
/// test) with the answer.
#[derive(Clone, Debug)]
struct ProjLine {
    set: usize,
    wp: usize,
    point: Vector3,
    answer: bool,
}

/// The `FIND` / `FINDIGN` truth lines: the probe point, the
/// `ignoreHeight` flag and the answered waypoint.
#[derive(Clone, Debug)]
struct FindLine {
    point: Vector3,
    ignore_height: bool,
    answer: i32,
}

struct Truth {
    headers: [(i32, f32, usize, usize); 2],
    set_counts: [(usize, i32); 2],
    wps: Vec<WpLine>,
    edges: Vec<EdgeLine>,
    adj: [(usize, usize, i32); 2],
    chunk_bound: (usize, usize, bool),
    vista: [u16; 3],
    contains: Vec<ContainsLine>,
    proj: Vec<ProjLine>,
    find: Vec<FindLine>,
}

fn parse_truth(lines: &[String]) -> Truth {
    let mut headers = [(0, 0.0, 0, 0); 2];
    let mut set_counts = [(0, 0); 2];
    let mut wps = Vec::new();
    let mut edges = Vec::new();
    let mut adj = [(0, 0, 0); 2];
    let mut chunk_bound = (0, 0, false);
    let mut vista = [0u16; 3];
    let mut contains = Vec::new();
    let mut proj = Vec::new();
    let mut find = Vec::new();
    let mut hdr_seen = 0;
    let mut set_seen = 0;
    let mut adj_seen = 0;
    let mut vista_seen = false;

    for line in lines {
        let f: Vec<&str> = line.split_whitespace().collect();
        match f[0] {
            "HDR0" | "HDR1" => {
                let slot = if f[0] == "HDR0" { 0 } else { 1 };
                // "HDR0 version 0 girth 0.600000 waypoints 3 edges 11"
                headers[slot] = (
                    f[2].parse().unwrap(),
                    f[4].parse().unwrap(),
                    f[6].parse().unwrap(),
                    f[8].parse().unwrap(),
                );
                hdr_seen += 1;
            }
            "SET0" | "SET1" => {
                let slot = if f[0] == "SET0" { 0 } else { 1 };
                // "SET0 count 3 base 1"
                set_counts[slot] = (f[2].parse().unwrap(), f[4].parse().unwrap());
                set_seen += 1;
            }
            "WP" => wps.push(WpLine {
                id: f[1].parse().unwrap(),
                minh: f[3].parse().unwrap(),
                maxh: f[5].parse().unwrap(),
                edges: f[7].parse().unwrap(),
                centre: (f[9].parse().unwrap(), f[10].parse().unwrap()),
            }),
            "EDGE" => edges.push(EdgeLine {
                vertex_index: f[3].parse().unwrap(),
                x: f[4].parse().unwrap(),
                y: f[5].parse().unwrap(),
                neighbour: u16::from_str_radix(f[6], 16).unwrap(),
            }),
            "ADJ" => {
                adj[adj_seen] = (
                    f[1].parse().unwrap(),
                    f[2].parse().unwrap(),
                    f[3].parse().unwrap(),
                );
                adj_seen += 1;
            }
            "CHUNKBOUND" => {
                // "CHUNKBOUND 0 0 1": wp, edge, and the chunk-adjacent flag.
                chunk_bound = (
                    f[1].parse().unwrap(),
                    f[2].parse().unwrap(),
                    f[3].parse::<u8>().unwrap() != 0,
                );
            }
            "VISTA" => {
                vista = [
                    u16::from_str_radix(f[1], 16).unwrap(),
                    u16::from_str_radix(f[2], 16).unwrap(),
                    u16::from_str_radix(f[3], 16).unwrap(),
                ];
                vista_seen = true;
            }
            "CONTAINS" => {
                // "CONTAINS <set> <wp> <x> <y> <z> <0/1>": waypoint 3 is
                // set 1's lone quad (set 0 has three).
                let set_id = f[1].parse::<usize>().unwrap();
                let wp_id = f[2].parse::<usize>().unwrap();
                contains.push(ContainsLine {
                    set: set_id,
                    wp: wp_id,
                    point: Vector3::new(
                        f[3].parse().unwrap(),
                        f[4].parse().unwrap(),
                        f[5].parse().unwrap(),
                    ),
                    answer: f[6].parse::<u8>().unwrap() != 0,
                });
            }
            "CONTAINSPROJ" => {
                // "CONTAINSPROJ <set> <wp> <x> <z> <0/1>" — always set 0's
                // own waypoint.
                proj.push(ProjLine {
                    set: f[1].parse().unwrap(),
                    wp: f[2].parse().unwrap(),
                    point: Vector3::new(f[3].parse().unwrap(), 0.0, f[4].parse().unwrap()),
                    answer: f[5].parse::<u8>().unwrap() != 0,
                });
            }
            "FIND" | "FINDIGN" => {
                // "FIND <x> <y> <z> <idx>", "FINDIGN <x> <y> <z> <idx>"
                find.push(FindLine {
                    point: Vector3::new(
                        f[1].parse().unwrap(),
                        f[2].parse().unwrap(),
                        f[3].parse().unwrap(),
                    ),
                    ignore_height: f[0] == "FINDIGN",
                    answer: f[4].parse().unwrap(),
                });
            }
            _ => {}
        }
    }

    assert_eq!(hdr_seen, 2, "two set headers in the truth");
    assert_eq!(set_seen, 2, "two set summaries in the truth");
    assert_eq!(adj_seen, 2, "two adjacency probes in the truth");
    assert!(vista_seen, "the vista probe must be present");
    assert!(
        !contains.is_empty(),
        "the containment probes must be present"
    );
    assert!(!proj.is_empty(), "the projection probes must be present");
    assert!(!find.is_empty(), "the find probes must be present");
    assert!(
        lines.last().unwrap().starts_with("RESULT OK"),
        "truth ends OK"
    );
    Truth {
        headers,
        set_counts,
        wps,
        edges,
        adj,
        chunk_bound,
        vista,
        contains,
        proj,
        find,
    }
}

fn close(a: f32, truth: f32) -> bool {
    // The truth prints %.6f.
    (a - truth).abs() <= 1e-5 * truth.abs().max(1.0)
}

#[test]
fn the_world_navmesh_fixture_restores_the_old_graph() {
    let blob = fixture_bytes("world_navmesh.bin");
    let truth = parse_truth(&truth_lines());

    let sets = parse_world_navmesh(&blob).expect("the fixture decodes");
    assert_eq!(sets.len(), 2, "the fixture holds two girth sets");

    // HDR lines: version 0, girth, waypoint and edge counts per set.
    for (slot, set) in sets.iter().enumerate() {
        let (version, girth, num_waypoints, num_edges) = truth.headers[slot];
        assert_eq!(version, 0);
        assert!(close(set.girth(), girth), "set {slot} girth");
        assert_eq!(set.waypoints().len(), num_waypoints, "set {slot} waypoints");
        assert_eq!(set.edges().len(), num_edges, "set {slot} edges");
    }

    // SET lines: counts and the accumulating base index ("yes, it starts
    // from 1"; 1 + 3 waypoints = 4).
    for (slot, set) in sets.iter().enumerate() {
        let (count, base) = truth.set_counts[slot];
        assert_eq!(set.waypoints().len(), count, "set {slot} count");
        assert_eq!(set.base_index(), base, "set {slot} base index");
    }

    // WP lines (per-set local ids, in record order): heights, edge run and
    // the calcCentre result.
    assert_eq!(
        truth.wps.len(),
        truth.headers[0].2 + truth.headers[1].2,
        "one WP line per waypoint"
    );
    let mut wp_cursor = 0usize;
    for set in &sets {
        for (local, wp) in set.waypoints().iter().enumerate() {
            let line = &truth.wps[wp_cursor];
            assert_eq!(line.id, local, "wp ids come back in record order");
            assert!(close(wp.min_height, line.minh), "wp {local} min height");
            assert!(close(wp.max_height, line.maxh), "wp {local} max height");
            assert_eq!(wp.edge_count as usize, line.edges, "wp {local} edge count");
            assert!(close(wp.centre.x, line.centre.0), "wp {local} centre x");
            assert!(close(wp.centre.y, line.centre.1), "wp {local} centre y");
            assert_eq!(wp.visited.get(), 0, "the decode resets visited_");
            wp_cursor += 1;
        }
    }

    // EDGE lines (set 0 owns ids 0..3, set 1 the lone quad): vertex index,
    // the resolved vertex coordinates and the raw neighbour encoding —
    // including the vertex dedup ((2,2) shared by A/B/C resolves to index
    // 1, (4,0) to index 4). The wp ids are per-set local and the lines
    // print set 0's 11 edges first, so the walk follows the record order.
    let mut edge_cursor = 0usize;
    for (slot, set) in sets.iter().enumerate() {
        assert_eq!(
            set.edges().len(),
            truth.headers[slot].3,
            "set {slot} edge count"
        );
        for wp in set.waypoints() {
            for local in 0..wp.edge_count as usize {
                let line = &truth.edges[edge_cursor];
                edge_cursor += 1;
                let edge = &set.edges()[wp.edge_range().start + local];
                assert_eq!(edge.vertex_index, line.vertex_index, "EDGE {line:?}");
                let v = set.vertex_by_index(edge.vertex_index);
                assert!(close(v.x, line.x) && close(v.y, line.y), "EDGE {line:?}");
                assert_eq!(edge.neighbour, line.neighbour, "EDGE {line:?}");
            }
        }
    }
    assert_eq!(edge_cursor, truth.edges.len());

    // ADJ lines (set 0's probes): the in-set link resolves to the waypoint
    // id, the boundary encoding answers otherChunk (-1).
    for (wp, edge, neighbour) in truth.adj {
        let w = &sets[0].waypoints()[wp];
        let e = &sets[0].edges()[w.edge_range().start + edge];
        assert_eq!(e.neighbouring_waypoint(), neighbour, "ADJ {wp} {edge}");
    }

    // CHUNKBOUND (set 0's waypoint A, edge 0): the boundary encoding 65535
    // reports chunk-adjacent.
    let (wp, edge, bound) = truth.chunk_bound;
    let w = &sets[0].waypoints()[wp];
    let e = &sets[0].edges()[w.edge_range().start + edge];
    assert_eq!(e.neighbour, 65535);
    assert_eq!(e.adjacent_to_chunk(), bound);

    // VISTA: the raw top-bit cover-flag encodings survive the round trip.
    let vista_of = |set: usize, wp: usize, edge: usize| {
        let w = &sets[set].waypoints()[wp];
        sets[set].edges()[w.edge_range().start + edge].neighbour
    };
    assert_eq!(vista_of(0, 0, 1), truth.vista[0]);
    assert_eq!(vista_of(0, 1, 0), truth.vista[1]);
    assert_eq!(vista_of(0, 2, 1), truth.vista[2]);

    // The 260-byte record decodes with nothing left over (the fixture is
    // exactly two set records).
    assert_eq!(blob.len(), 260);
}

#[test]
fn the_fixture_vertices_dedup_across_waypoints() {
    let blob = fixture_bytes("world_navmesh.bin");
    let sets = parse_world_navmesh(&blob).expect("the fixture decodes");

    let set = &sets[0];
    assert_eq!(set.vertices().len(), 6, "four shared corners + two extras");
    // The clockwise winding puts A's first vertex at (0,2); the shared
    // corner (2,2) comes second.
    assert_eq!(
        set.vertices(),
        [
            Vector2::new(0.0, 2.0),
            Vector2::new(2.0, 2.0),
            Vector2::new(2.0, 0.0),
            Vector2::new(0.0, 0.0),
            Vector2::new(4.0, 0.0),
            Vector2::new(4.0, 2.0),
        ]
    );

    // Set 1's table restarts (per-set vertices).
    let set1 = &sets[1];
    assert_eq!(
        set1.vertices(),
        [
            Vector2::new(10.0, 12.0),
            Vector2::new(12.0, 12.0),
            Vector2::new(12.0, 10.0),
            Vector2::new(10.0, 10.0),
        ]
    );

    // Waypoint A links B twice over the shared corner pair; C links B and
    // carries the vista edge; the boundary edge is the first of A and C.
    let a_edges = set.waypoint_edges(0);
    assert_eq!(
        a_edges[0].neighbour, 65535,
        "A's first edge is the boundary"
    );
    assert_eq!(a_edges[2].neighbouring_waypoint(), 1);
    assert_eq!(a_edges[3].neighbouring_waypoint(), 1);

    let b_edges = set.waypoint_edges(1);
    assert_eq!(b_edges[1].neighbouring_waypoint(), 0, "back to A");
    assert_eq!(b_edges[2].neighbouring_waypoint(), 2);
    assert_eq!(b_edges[3].neighbouring_waypoint(), 2);

    let c_edges = set.waypoint_edges(2);
    assert_eq!(
        c_edges[0].neighbour, 65535,
        "C's first edge is the boundary"
    );
    assert_eq!(c_edges[2].neighbouring_waypoint(), 1);

    // The absolute edge index maps an edge to its shared-array slot.
    assert_eq!(set.absolute_edge_index(0, 0), 0);
    assert_eq!(set.absolute_edge_index(1, 0), 4);
    assert_eq!(set.absolute_edge_index(2, 2), 10);
}

#[test]
fn the_reference_containment_and_projection_probes_are_answered() {
    let blob = fixture_bytes("world_navmesh.bin");
    let truth = parse_truth(&truth_lines());
    let sets = parse_world_navmesh(&blob).expect("the fixture decodes");

    // CONTAINS <set> <wp> x y z answer — the height range plus the
    // projection test.
    for line in &truth.contains {
        let set = &sets[line.set];
        let wp = &set.waypoints()[line.wp];
        assert_eq!(
            wp.contains(set.edges(), set, line.point),
            line.answer,
            "CONTAINS {} {:?} answer",
            line.wp,
            line.point
        );
    }

    // CONTAINSPROJ <set> <wp> x z answer — the projection test alone,
    // which is what the multi-layer search keys on.
    for line in &truth.proj {
        let set = &sets[line.set];
        let wp = &set.waypoints()[line.wp];
        assert_eq!(
            wp.contains_projection(set.edges(), set, line.point),
            line.answer,
            "CONTAINSPROJ {} {:?} answer",
            line.wp,
            line.point
        );
    }
}

#[test]
fn the_reference_find_probes_pick_the_right_layer() {
    let blob = fixture_bytes("world_navmesh.bin");
    let truth = parse_truth(&truth_lines());
    let sets = parse_world_navmesh(&blob).expect("the fixture decodes");

    // FIND / FINDIGN: the containing waypoint, or -1 when nothing
    // contains the point. The ignoreHeight form answers the best height
    // fit among the projection-containing waypoints, which is how the
    // multi-layer stack resolves.
    for line in &truth.find {
        assert_eq!(
            sets[0].find(line.point, line.ignore_height),
            line.answer,
            "FIND {:?} ignore_height={} answer",
            line.point,
            line.ignore_height
        );
    }
}
