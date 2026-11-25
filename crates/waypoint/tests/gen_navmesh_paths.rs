//! BW-10-05-05 acceptance: the ORIGINAL generator's saved navmesh assets are
//! read back and the fixed maps path, with the original engine as the truth
//! side.
//!
//! The sample assets are not hand-authored polygons: `gen_generate.cpp` runs
//! the unmodified `waypoint_flood.cpp` + `waypoint_generator.cpp` +
//! `waypoint_view.cpp` pipeline over the two fixed scenes (the 0.5-step slope
//! and the indoor shell with its stacked bridge) and hex-dumps the FINAL
//! record stream — exactly the bytes navgen saves into
//! `.cdata/worldNavmesh` — into `gen_generate.log`. `gen_navpaths.cpp` loads
//! those bytes through the original
//! `ChunkWaypointSetData::readWaypointSet` face and runs the original
//! `NavLoc`/`AStar` over seven fixed start/end pairs, printing
//! `gen_navpaths.log`.
//!
//! This test decodes the same bytes through the port, answers the same
//! containment/find probes, and replays every reference path step for step.

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

/// One probe of the harness's `PathProbe` blocks: the fixed start/end
/// points of a reference path.
struct ProbePoint {
    src: Vector3,
    dst: Vector3,
}

/// The probe table, the same constants as `gen_navpaths.cpp`'s `PathProbe`
/// blocks, keyed by (scene, name).
const PROBES: [(&str, &str, ProbePoint); 7] = [
    // across the shared x = 3 edge (P1 -> P0) and back
    (
        "slope",
        "S1",
        ProbePoint {
            src: Vector3::new(1.0, 0.5, 1.0),
            dst: Vector3::new(7.0, 3.0, 7.0),
        },
    ),
    (
        "slope",
        "S2",
        ProbePoint {
            src: Vector3::new(7.0, 3.0, 7.0),
            dst: Vector3::new(1.0, 0.5, 1.0),
        },
    ),
    // within one polygon
    (
        "slope",
        "S3",
        ProbePoint {
            src: Vector3::new(1.0, 0.5, 1.0),
            dst: Vector3::new(2.0, 0.5, 2.0),
        },
    ),
    (
        "slope",
        "S4",
        ProbePoint {
            src: Vector3::new(6.0, 3.0, 6.0),
            dst: Vector3::new(7.0, 3.0, 7.0),
        },
    ),
    // within the room floor (record 0's set)
    (
        "shell",
        "F1",
        ProbePoint {
            src: Vector3::new(2.5, 0.5, 4.5),
            dst: Vector3::new(3.5, 0.5, 2.5),
        },
    ),
    // within the bridge (record 1's set)
    (
        "shell",
        "B1",
        ProbePoint {
            src: Vector3::new(3.0, 1.5, 3.5),
            dst: Vector3::new(2.5, 1.5, 3.0),
        },
    ),
    // floor -> bridge: the generator gave the layers no inter-set
    // adjacency, so the search must fail
    (
        "shell",
        "X1",
        ProbePoint {
            src: Vector3::new(2.5, 0.5, 4.5),
            dst: Vector3::new(3.0, 1.5, 3.5),
        },
    ),
];

/// The `HX` hex dump under `"<label> bytes N"` in the generator truth log is
/// the asset; the label names the record stream as the harness dumped it (the
/// two FINAL streams, after streamline and set membership).
fn final_stream(label: &str) -> Vec<u8> {
    let log = std::fs::read_to_string(format!("{FIXTURE_DIR}/gen_generate.log"))
        .expect("the generator truth log");
    let mut out = Vec::new();
    let mut declared: Option<usize> = None;
    let mut armed = false;
    for line in log.lines() {
        if armed && !line.starts_with("HX ") {
            break;
        }
        if !armed && line.starts_with(&format!("{label} bytes ")) {
            declared = Some(
                line.split_whitespace()
                    .nth(2)
                    .expect("declared size")
                    .parse()
                    .unwrap(),
            );
            armed = true;
            continue;
        }
        if !armed {
            continue;
        }
        let hex = line.split_whitespace().nth(2).expect("hex column");
        assert!(hex.len() % 2 == 0, "HX rows carry whole bytes: {line}");
        for pair in (0..hex.len()).step_by(2) {
            out.push(u8::from_str_radix(&hex[pair..pair + 2], 16).expect("hex byte"));
        }
    }
    assert!(armed, "the {label} stream is in the log");
    assert_eq!(declared, Some(out.len()), "the {label} dump is complete");
    out
}

fn navpaths_lines() -> Vec<String> {
    std::fs::read_to_string(format!("{FIXTURE_DIR}/gen_navpaths.log"))
        .expect("the navpaths truth log")
        .lines()
        .map(str::to_owned)
        .collect()
}

/// The `LHDR` truth lines: the record header the original reader walked.
#[derive(Clone, Debug)]
struct RecordHeader {
    scene: String,
    record: usize,
    version: i32,
    girth: f32,
    waypoints: usize,
    edges: usize,
}

/// The `LWP` truth lines: heights, edge count and the reader's centre.
#[derive(Clone, Debug)]
struct LoadedWaypoint {
    scene: String,
    record: usize,
    index: usize,
    minh: f32,
    maxh: f32,
    edges: usize,
    centre: (f32, f32),
}

/// The `LEDGE` truth lines: vertex coordinates and the raw neighbour
/// encoding (FFFF is no adjacency, 0001 the slope's P0<->P1 link).
#[derive(Clone, Debug)]
struct LoadedEdge {
    scene: String,
    record: usize,
    wp: usize,
    edge: usize,
    x: f32,
    y: f32,
    neighbour: u16,
}

/// The `CONTAINS` truth lines: set-local containment at a fixed point.
#[derive(Clone, Debug)]
struct LayerProbe {
    scene: String,
    set: usize,
    point: Vector3,
    answer: bool,
}

/// The `FIND` truth lines: the containing waypoint of the set, or -1.
#[derive(Clone, Debug)]
struct FindProbe {
    scene: String,
    set: usize,
    point: Vector3,
    answer: i32,
}

/// One reference path: the resolved src/dst waypoints plus the printed steps.
/// An empty step list is the reference search failing outright (PATHNONE).
#[derive(Clone, Debug)]
struct RefPath {
    scene: String,
    name: String,
    src: Vector3,
    dst: Vector3,
    src_wp: i32,
    dst_wp: i32,
    steps: Vec<(f64, f64, f64, i32)>,
}

struct Truth {
    records: Vec<(String, usize)>,
    headers: Vec<RecordHeader>,
    wps: Vec<LoadedWaypoint>,
    edges: Vec<LoadedEdge>,
    contains: Vec<LayerProbe>,
    find: Vec<FindProbe>,
    paths: Vec<RefPath>,
}

fn parse_truth(lines: &[String]) -> Truth {
    let mut truth = Truth {
        records: Vec::new(),
        headers: Vec::new(),
        wps: Vec::new(),
        edges: Vec::new(),
        contains: Vec::new(),
        find: Vec::new(),
        paths: Vec::new(),
    };
    for line in lines {
        let f: Vec<&str> = line.split_whitespace().collect();
        match f[0] {
            "LOAD" => truth.records.push((f[1].to_owned(), f[3].parse().unwrap())),
            "LHDR" => truth.headers.push(RecordHeader {
                scene: f[1].to_owned(),
                record: f[2].parse().unwrap(),
                version: f[4].parse().unwrap(),
                girth: f[6].parse().unwrap(),
                waypoints: f[8].parse().unwrap(),
                edges: f[10].parse().unwrap(),
            }),
            "LWP" => truth.wps.push(LoadedWaypoint {
                scene: f[1].to_owned(),
                record: f[2].parse().unwrap(),
                index: f[3].parse().unwrap(),
                minh: f[5].parse().unwrap(),
                maxh: f[7].parse().unwrap(),
                edges: f[9].parse().unwrap(),
                centre: (f[11].parse().unwrap(), f[12].parse().unwrap()),
            }),
            "LEDGE" => truth.edges.push(LoadedEdge {
                scene: f[1].to_owned(),
                record: f[2].parse().unwrap(),
                wp: f[3].parse().unwrap(),
                edge: f[4].parse().unwrap(),
                x: f[5].parse().unwrap(),
                y: f[6].parse().unwrap(),
                neighbour: u16::from_str_radix(f[7], 16).unwrap(),
            }),
            "CONTAINS" => truth.contains.push(LayerProbe {
                scene: f[1].to_owned(),
                set: f[2].parse().unwrap(),
                point: Vector3::new(
                    f[4].parse().unwrap(),
                    f[5].parse().unwrap(),
                    f[6].parse().unwrap(),
                ),
                answer: f[7].parse::<u8>().unwrap() != 0,
            }),
            "FIND" => truth.find.push(FindProbe {
                scene: f[1].to_owned(),
                set: f[2].parse().unwrap(),
                point: Vector3::new(
                    f[4].parse().unwrap(),
                    f[5].parse().unwrap(),
                    f[6].parse().unwrap(),
                ),
                answer: f[7].parse().unwrap(),
            }),
            "PATHBEGIN" => {
                let (src, dst) = PROBES
                    .iter()
                    .find(|(scene, name, ..)| *scene == f[1] && *name == f[2])
                    .map(|(_, _, p)| (p.src, p.dst))
                    .expect("every truth path names a probe");
                truth.paths.push(RefPath {
                    scene: f[1].to_owned(),
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
    assert_eq!(truth.records.len(), 2, "two fixed scenes");
    assert_eq!(
        truth.paths.len(),
        PROBES.len(),
        "every probe has truth lines"
    );
    assert!(!truth.contains.is_empty(), "layer probes present");
    assert!(!truth.find.is_empty(), "find probes present");
    truth
}

fn close(a: f32, truth: f32) -> bool {
    // The truth prints %.6f.
    (a - truth).abs() <= 1e-5 * truth.abs().max(1.0)
}

/// Both scenes decoded through the port: slope's one record and the shell's
/// two same-girth records.
fn decoded() -> (Vec<OwnedSet>, Vec<OwnedSet>) {
    (
        parse_world_navmesh(&final_stream("slope"))
            .expect("the slope asset decodes")
            .into_iter()
            .map(Rc::new)
            .collect(),
        parse_world_navmesh(&final_stream("shell"))
            .expect("the shell asset decodes")
            .into_iter()
            .map(Rc::new)
            .collect(),
    )
}

fn sets_of<'a>(scene: &str, decoded: &'a (Vec<OwnedSet>, Vec<OwnedSet>)) -> &'a [OwnedSet] {
    match scene {
        "slope" => &decoded.0,
        "shell" => &decoded.1,
        other => panic!("unknown scene {other}"),
    }
}

#[test]
fn the_generated_asset_loads_through_the_reference_reader_face() {
    let truth = parse_truth(&navpaths_lines());
    let decoded = decoded();

    // LOAD lines: the asset holds exactly the record count the reference
    // walk counted (records accumulate over the same stream).
    for (scene, count) in &truth.records {
        assert_eq!(
            sets_of(scene, &decoded).len(),
            *count,
            "{scene} record count"
        );
    }
    assert_eq!(truth.records.len(), 2, "two scenes loaded");

    // LHDR lines: the per-record header the original reader walked.
    for hdr in &truth.headers {
        let set = &*sets_of(&hdr.scene, &decoded)[hdr.record];
        assert_eq!(hdr.version, 0, "the record version");
        assert!(
            close(set.girth(), hdr.girth),
            "{}/{} girth",
            hdr.scene,
            hdr.record
        );
        assert_eq!(
            set.waypoints().len(),
            hdr.waypoints,
            "{}/{} waypoints",
            hdr.scene,
            hdr.record
        );
        assert_eq!(
            set.edges().len(),
            hdr.edges,
            "{}/{} edges",
            hdr.scene,
            hdr.record
        );
    }

    // LWP lines: heights, edge count and the centre the reader computed.
    for line in &truth.wps {
        let set = &*sets_of(&line.scene, &decoded)[line.record];
        let wp = &set.waypoints()[line.index];
        assert!(close(wp.min_height, line.minh), "LWP {:?} minh", line);
        assert!(close(wp.max_height, line.maxh), "LWP {:?} maxh", line);
        assert_eq!(wp.edge_count as usize, line.edges, "LWP {:?} edges", line);
        assert!(close(wp.centre.x, line.centre.0), "LWP {:?} centre x", line);
        assert!(close(wp.centre.y, line.centre.1), "LWP {:?} centre y", line);
    }

    // LEDGE lines: vertex coordinates and the raw neighbour encoding.
    for line in &truth.edges {
        let set = &*sets_of(&line.scene, &decoded)[line.record];
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
}

#[test]
fn the_fixed_maps_answer_the_layer_probes() {
    let truth = parse_truth(&navpaths_lines());
    let decoded = decoded();

    // The reference probes each set's first waypoint: the height band is
    // per set, so the floor point misses the bridge layer and vice versa.
    for line in &truth.contains {
        let set = &*sets_of(&line.scene, &decoded)[line.set];
        let answer = set
            .waypoints()
            .first()
            .is_some_and(|wp| wp.contains(set.edges(), set, line.point));
        assert_eq!(
            answer, line.answer,
            "CONTAINS {} set {} at {:?}",
            line.scene, line.set, line.point
        );
    }

    for line in &truth.find {
        let set = &*sets_of(&line.scene, &decoded)[line.set];
        assert_eq!(
            set.find(line.point, false),
            line.answer,
            "FIND {} set {} at {:?}",
            line.scene,
            line.set,
            line.point
        );
    }
}

/// The reference `NavLoc` constructor resolution: the height-honouring
/// containment first, then the closest-waypoint fallback, with the same
/// +0.01 lift the constructor applies.
fn resolve(scene: &str, at: Vector3, decoded: &(Vec<OwnedSet>, Vec<OwnedSet>)) -> (OwnedSet, i32) {
    let point = Vector3::new(at.x, at.y + 0.01, at.z);
    let sets = sets_of(scene, decoded);
    for set in sets {
        let found = set.find(point, false);
        if found >= 0 {
            return (Rc::clone(set), found);
        }
    }
    let mut best = f32::MAX;
    for set in sets {
        let wp = set.find_closest(point, &mut best);
        if wp >= 0 {
            return (Rc::clone(set), wp);
        }
    }
    (
        Rc::clone(sets.first().expect("a scene loads at least one set")),
        -1,
    )
}

#[test]
fn the_fixed_maps_path_point_by_point_against_the_reference() {
    let truth = parse_truth(&navpaths_lines());
    let decoded = decoded();

    let mut reachable = 0usize;
    let mut unreachable = 0usize;
    for probe in &truth.paths {
        let (src_set, src_wp) = resolve(&probe.scene, probe.src, &decoded);
        let (dst_set, dst_wp) = resolve(&probe.scene, probe.dst, &decoded);
        assert_eq!(
            src_wp, probe.src_wp,
            "PATHBEGIN {} {}",
            probe.scene, probe.name
        );
        assert_eq!(
            dst_wp, probe.dst_wp,
            "PATHBEGIN {} {}",
            probe.scene, probe.name
        );

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
                "PATHNONE {} {} — the reference search failed, so the port's \
                 must too",
                probe.scene, probe.name
            );
            unreachable += 1;
            continue;
        }
        assert!(searched, "search {} {}", probe.scene, probe.name);
        reachable += 1;

        let steps = astar.path();
        assert_eq!(
            steps.len(),
            probe.steps.len(),
            "path length {} {}",
            probe.scene,
            probe.name
        );
        for (i, want) in probe.steps.iter().enumerate() {
            let p = steps[i].nav_loc().point();
            assert_eq!(
                steps[i].nav_loc().waypoint(),
                want.3,
                "PATH {} {} step {} waypoint",
                probe.scene,
                probe.name,
                i
            );
            assert!(
                (p.x as f64 - want.0).abs() <= 1e-6
                    && (p.y as f64 - want.1).abs() <= 1e-6
                    && (p.z as f64 - want.2).abs() <= 1e-6,
                "PATH {} {} step {} point ({:.6},{:.6},{:.6}) vs ({},{},{})",
                probe.scene,
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
    assert!(reachable >= 4, "the reachable pairs all path");
    assert!(unreachable >= 1, "the disconnected layers report no path");
}
