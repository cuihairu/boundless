//! BW-10-07-02 acceptance, consumer leg: the fixed two-chunk map the
//! navgen pipeline edited and generated (`gen_navgen_map.cpp` over the
//! port, byte-identical to the reference export) loads through the
//! port's `ChunkWaypointSetData::readWaypointSet` face and answers the
//! same containment / find / NavLoc / A* probes the original reader
//! answered in `gen_navgen_paths.log`.
//!
//! The two committed `.bin` assets are the reference harness's export
//! bytes (mapA 496 bytes fnv 3f4c4a26e9865b1f, mapB 640 bytes fnv
//! c9b5fe1853f86a48); the generation replay in waypoint-gen pins the
//! port's export to the same fnv, and the el7 `gen_navgen_paths`
//! binary run against the port's bytes printed this log verbatim.

use foundation::math::Vector3;
use std::rc::Rc;
use waypoint::{
    AStar, ChunkWaypointSetData, ChunkWaypointState, ChunkWaypointVertexProvider, NavLoc,
    parse_world_navmesh,
};

type OwnedSet = Rc<ChunkWaypointSetData>;

const FIXTURE_DIR: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../docs/review/fixtures/waypoint"
);

fn asset(name: &str) -> Vec<u8> {
    std::fs::read(format!("{FIXTURE_DIR}/{name}")).unwrap_or_else(|e| panic!("read {name}: {e}"))
}

fn decoded() -> (Vec<OwnedSet>, Vec<OwnedSet>) {
    let a = parse_world_navmesh(&asset("world_navmesh_genmap_a.bin")).expect("mapA decodes");
    let b = parse_world_navmesh(&asset("world_navmesh_genmap_b.bin")).expect("mapB decodes");
    (
        a.into_iter().map(Rc::new).collect(),
        b.into_iter().map(Rc::new).collect(),
    )
}

fn genmap_lines() -> Vec<String> {
    std::fs::read_to_string(format!("{FIXTURE_DIR}/gen_navgen_paths.log"))
        .expect("the genmap truth log")
        .lines()
        .map(str::to_owned)
        .collect()
}

/// The `LHDR` truth lines: the record header the original reader walked.
#[derive(Clone, Debug)]
struct RecordHeader {
    asset: String,
    version: i32,
    girth: f32,
    waypoints: usize,
    edges: usize,
}

/// The `LWP` truth lines: heights, edge count and the reader's centre.
#[derive(Clone, Debug)]
struct LoadedWaypoint {
    asset: String,
    index: usize,
    minh: f32,
    maxh: f32,
    edges: usize,
    centre: (f32, f32),
}

/// The `LEDGE` truth lines: vertex coordinates and the raw neighbour
/// encoding (FFFF is the seam's another-chunk edge, wired only through
/// portals in 07-03).
#[derive(Clone, Debug)]
struct LoadedEdge {
    asset: String,
    wp: usize,
    edge: usize,
    x: f32,
    y: f32,
    neighbour: u16,
}

/// The `FIND` truth lines: the containing waypoint of the set, or -1.
#[derive(Clone, Debug)]
struct FindProbe {
    set: usize,
    point: Vector3,
    answer: i32,
}

/// The `NAVLOC` truth lines: the constructor resolution per nav point.
#[derive(Clone, Debug)]
struct NavLocProbe {
    point: Vector3,
    wp: i32,
    y: f32,
}

/// One path probe: the fixed start/end points plus the printed steps.
/// An empty step list is the reference search failing outright
/// (PATHNONE).
#[derive(Clone, Debug)]
struct RefPath {
    name: String,
    src: Vector3,
    dst: Vector3,
    src_wp: i32,
    dst_wp: i32,
    steps: Vec<(f64, f64, f64, i32)>,
}

struct Truth {
    loads: Vec<(String, usize)>,
    headers: Vec<RecordHeader>,
    wps: Vec<LoadedWaypoint>,
    edges: Vec<LoadedEdge>,
    find: Vec<FindProbe>,
    navlocs: Vec<NavLocProbe>,
    paths: Vec<RefPath>,
}

/// The nine fixed nav points of the harness (`gen_navgen_paths.cpp`).
const NAV_PTS: [Vector3; 9] = [
    Vector3::new(1.0, 4.0, 1.0),
    Vector3::new(4.0, 4.0, 1.0),
    Vector3::new(2.0, 8.0, 8.0),
    Vector3::new(2.5, 4.0, 1.0),
    Vector3::new(6.0, 4.0, 2.0),
    Vector3::new(6.0, 4.0, 8.0),
    Vector3::new(5.5, 4.0, 5.0),
    Vector3::new(7.5, 4.0, 5.0),
    Vector3::new(5.0, 4.0, 2.0),
];

/// The four path probes of the harness, keyed by name.
fn probe_points(name: &str) -> (Vector3, Vector3) {
    match name {
        // across chunk A west->east: the hole forces the ramp detour
        "ACROSS" => (NAV_PTS[0], NAV_PTS[1]),
        // west ground up to the ramp top band
        "RAMP" => (NAV_PTS[0], NAV_PTS[2]),
        // across chunk B south->north around the obstacle absence
        "AROUND" => (NAV_PTS[4], NAV_PTS[5]),
        // over the chunk seam: the 65535 edges are unwired, must fail
        "SEAM" => (NAV_PTS[0], NAV_PTS[4]),
        other => panic!("unknown probe {other}"),
    }
}

fn parse_truth(lines: &[String]) -> Truth {
    let mut truth = Truth {
        loads: Vec::new(),
        headers: Vec::new(),
        wps: Vec::new(),
        edges: Vec::new(),
        find: Vec::new(),
        navlocs: Vec::new(),
        paths: Vec::new(),
    };
    for line in lines {
        let f: Vec<&str> = line.split_whitespace().collect();
        match f[0] {
            "LOAD" => truth.loads.push((f[1].to_owned(), f[3].parse().unwrap())),
            "LHDR" => truth.headers.push(RecordHeader {
                asset: f[1].to_owned(),
                version: f[4].parse().unwrap(),
                girth: f[6].parse().unwrap(),
                waypoints: f[8].parse().unwrap(),
                edges: f[10].parse().unwrap(),
            }),
            "LWP" => truth.wps.push(LoadedWaypoint {
                asset: f[1].to_owned(),
                index: f[3].parse().unwrap(),
                minh: f[5].parse().unwrap(),
                maxh: f[7].parse().unwrap(),
                edges: f[9].parse().unwrap(),
                centre: (f[11].parse().unwrap(), f[12].parse().unwrap()),
            }),
            "LEDGE" => truth.edges.push(LoadedEdge {
                asset: f[1].to_owned(),
                wp: f[3].parse().unwrap(),
                edge: f[4].parse().unwrap(),
                x: f[5].parse().unwrap(),
                y: f[6].parse().unwrap(),
                neighbour: u16::from_str_radix(f[7], 16).unwrap(),
            }),
            "FIND" => truth.find.push(FindProbe {
                set: f[2].parse().unwrap(),
                point: Vector3::new(
                    f[4].parse().unwrap(),
                    f[5].parse().unwrap(),
                    f[6].parse().unwrap(),
                ),
                answer: f[7].parse().unwrap(),
            }),
            "NAVLOC" => truth.navlocs.push(NavLocProbe {
                point: Vector3::new(
                    f[3].parse().unwrap(),
                    f[4].parse().unwrap(),
                    f[5].parse().unwrap(),
                ),
                wp: f[7].parse().unwrap(),
                y: f[9].parse().unwrap(),
            }),
            "PATHBEGIN" => {
                let (src, dst) = probe_points(f[2]);
                truth.paths.push(RefPath {
                    name: f[2].to_owned(),
                    src,
                    dst,
                    src_wp: f[3].parse().unwrap(),
                    dst_wp: f[4].parse().unwrap(),
                    steps: Vec::new(),
                });
            }
            "PATH" => truth
                .paths
                .last_mut()
                .expect("PATH after PATHBEGIN")
                .steps
                .push((
                    f[3].parse().unwrap(),
                    f[4].parse().unwrap(),
                    f[5].parse().unwrap(),
                    f[6].parse().unwrap(),
                )),
            _ => {}
        }
    }
    assert_eq!(lines.last().unwrap().as_str(), "RESULT OK", "truth ends OK");
    assert_eq!(truth.loads.len(), 2, "both assets loaded");
    assert_eq!(truth.headers.len(), 2, "one record per asset");
    assert_eq!(truth.navlocs.len(), NAV_PTS.len(), "every nav point locs");
    assert_eq!(truth.paths.len(), 4, "four path probes");
    truth
}

fn close(a: f32, truth: f32) -> bool {
    // The truth prints %.6f.
    (a - truth).abs() <= 1e-5 * truth.abs().max(1.0)
}

fn sets_of<'a>(asset: &str, decoded: &'a (Vec<OwnedSet>, Vec<OwnedSet>)) -> &'a [OwnedSet] {
    match asset {
        "mapA" => &decoded.0,
        "mapB" => &decoded.1,
        other => panic!("unknown asset {other}"),
    }
}

/// The reference `NavLoc(Chunk*, point, girth)` resolution
/// (`navloc.cpp:66-89`) in the phase the port's set face models: the
/// height-honouring containment over every set the chunk mounts, in the
/// order the harness mounts them. The +0.01 lift is the constructor's.
///
/// A miss here is answered by the reference through the navigator's
/// girth-grid ring search (`chunk_navigator.cpp:156-232`), which stops
/// after the first ring that found a candidate; the port does not model
/// that index, so the two fallback points of the truth log are asserted
/// against the recorded ring answers instead of recomputed.
fn containment(at: Vector3, decoded: &(Vec<OwnedSet>, Vec<OwnedSet>)) -> Option<(OwnedSet, i32)> {
    let point = Vector3::new(at.x, at.y + 0.01, at.z);
    for sets in [&decoded.0, &decoded.1] {
        // one set per asset in this fixture
        let found = sets[0].find(point, false);
        if found >= 0 {
            return Some((Rc::clone(&sets[0]), found));
        }
    }
    None
}

/// The reference's ring-search answers for the two nav points no
/// polygon contains: the hole centre (2.5,4,1) and the obstacle centre
/// (7.5,4,5) — `gen_navgen_paths.log` NAVLOC lines 3 and 7.
const RING_ANSWERS: [(usize, i32); 2] = [(3, 3), (7, 8)];

#[test]
fn the_genmap_assets_load_through_the_port_reader_face() {
    let truth = parse_truth(&genmap_lines());
    let decoded = decoded();

    // LOAD lines: each asset holds exactly the record count the original
    // walk counted.
    for (name, count) in &truth.loads {
        assert_eq!(sets_of(name, &decoded).len(), *count, "{name} records");
    }

    // LHDR lines: the per-record header the original reader walked.
    for hdr in &truth.headers {
        let set = &*sets_of(&hdr.asset, &decoded)[0];
        assert_eq!(hdr.version, 0, "{} version", hdr.asset);
        assert!(close(set.girth(), hdr.girth), "{} girth", hdr.asset);
        assert_eq!(
            set.waypoints().len(),
            hdr.waypoints,
            "{} waypoints",
            hdr.asset
        );
        assert_eq!(set.edges().len(), hdr.edges, "{} edges", hdr.asset);
    }

    // LWP lines: heights, edge count and the centre the reader computed.
    for line in &truth.wps {
        let set = &*sets_of(&line.asset, &decoded)[0];
        let wp = &set.waypoints()[line.index];
        assert!(close(wp.min_height, line.minh), "LWP {:?} minh", line);
        assert!(close(wp.max_height, line.maxh), "LWP {:?} maxh", line);
        assert_eq!(wp.edge_count as usize, line.edges, "LWP {:?} edges", line);
        assert!(close(wp.centre.x, line.centre.0), "LWP {:?} centre x", line);
        assert!(close(wp.centre.y, line.centre.1), "LWP {:?} centre y", line);
    }

    // LEDGE lines: vertex coordinates and the raw neighbour encoding —
    // the 34 FFFF entries are the seam and boundary edges.
    for line in &truth.edges {
        let set = &*sets_of(&line.asset, &decoded)[0];
        let wp = &set.waypoints()[line.wp];
        let edge = &set.edges()[wp.edge_range().start + line.edge];
        let v = set.vertex_by_index(edge.vertex_index);
        assert!(
            close(v.x, line.x) && close(v.y, line.y),
            "LEDGE {:?} vertex",
            line
        );
        assert_eq!(edge.neighbour, line.neighbour, "LEDGE {:?} neighbour", line);
    }

    // FIND lines: per set, the containing waypoint or -1. The harness's
    // set index is its asset index; each asset holds one set.
    for line in &truth.find {
        let asset = if line.set == 0 { "mapA" } else { "mapB" };
        let set = &*sets_of(asset, &decoded)[0];
        assert_eq!(
            set.find(line.point, false),
            line.answer,
            "FIND set {} at {:?}",
            line.set,
            line.point
        );
    }
}

#[test]
fn the_genmap_navlocs_and_paths_replay_the_reference() {
    let truth = parse_truth(&genmap_lines());
    let decoded = decoded();

    // NAVLOC lines: every probe point resolves to the same waypoint, at
    // the constructor's lifted height.
    for (i, probe) in truth.navlocs.iter().enumerate() {
        assert_eq!(probe.point, NAV_PTS[i], "nav point {i} order");
        let y = probe.point.y + 0.01;
        assert!(close(y, probe.y), "NAVLOC {i} y {y} vs {}", probe.y);
        match containment(probe.point, &decoded) {
            Some((_, wp)) => assert_eq!(wp, probe.wp, "NAVLOC {i} waypoint"),
            None => {
                let ring_wp = RING_ANSWERS
                    .iter()
                    .find(|(point, ..)| *point == i)
                    .map(|(_, wp)| *wp)
                    .expect("the ring points are recorded");
                assert_eq!(probe.wp, ring_wp, "NAVLOC {i} ring answer");
            }
        }
    }

    let mut reachable = 0usize;
    let mut unreachable = 0usize;
    for probe in &truth.paths {
        // every probe point of this fixture lands in a polygon, so the
        // containment phase answers the constructor's resolution
        let (src_set, src_wp) = containment(probe.src, &decoded)
            .unwrap_or_else(|| panic!("PATHBEGIN {} src resolves", probe.name));
        let (dst_set, dst_wp) = containment(probe.dst, &decoded)
            .unwrap_or_else(|| panic!("PATHBEGIN {} dst resolves", probe.name));
        assert_eq!(src_wp, probe.src_wp, "PATHBEGIN {} src", probe.name);
        assert_eq!(dst_wp, probe.dst_wp, "PATHBEGIN {} dst", probe.name);

        let src_state = ChunkWaypointState::from_navloc(&NavLoc::from_parts(
            src_set,
            src_wp,
            Vector3::new(probe.src.x, probe.src.y + 0.01, probe.src.z),
            100.0,
        ));
        let dst_state = ChunkWaypointState::from_navloc(&NavLoc::from_parts(
            dst_set,
            dst_wp,
            Vector3::new(probe.dst.x, probe.dst.y + 0.01, probe.dst.z),
            100.0,
        ));

        let mut astar = AStar::default();
        let searched = astar.search(&src_state, &dst_state, -1.0);

        if probe.steps.is_empty() {
            assert!(
                !searched,
                "PATHNONE {} — the reference search failed across the \
                 unwired seam, so the port's must too",
                probe.name
            );
            unreachable += 1;
            continue;
        }
        assert!(searched, "search {}", probe.name);
        reachable += 1;

        let steps = astar.path();
        assert_eq!(steps.len(), probe.steps.len(), "path length {}", probe.name);
        for (i, want) in probe.steps.iter().enumerate() {
            let p = steps[i].nav_loc().point();
            assert_eq!(
                steps[i].nav_loc().waypoint(),
                want.3,
                "PATH {} step {} waypoint",
                probe.name,
                i
            );
            assert!(
                (p.x as f64 - want.0).abs() <= 1e-6
                    && (p.y as f64 - want.1).abs() <= 1e-6
                    && (p.z as f64 - want.2).abs() <= 1e-6,
                "PATH {} step {} point ({:.6},{:.6},{:.6}) vs ({},{},{})",
                probe.name,
                i,
                p.x,
                p.y,
                p.z,
                want.0,
                want.1,
                want.2
            );
        }
    }
    assert_eq!(reachable, 3, "ACROSS/RAMP/AROUND path");
    assert_eq!(unreachable, 1, "SEAM stays unwired in 07-02");
}
