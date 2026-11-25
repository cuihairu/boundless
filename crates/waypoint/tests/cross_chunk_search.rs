//! BW-10-04-05 integration: the cross-chunk fixture
//! (`docs/review/fixtures/waypoint/world_navmesh_cross.bin`) must wire up
//! and search through the port exactly as the reference engine printed in
//! `gen_waypoints_cross.log`:
//!
//! - three inside chunks in a line (A 1 waypoint, B 2, C 1), portals A|B
//!   non-permissive and B|C permissive, all four directions bound;
//! - the `connect()` wiring replays the truth `CONNTO`/`EDGELABEL` lines;
//! - the freshly constructed `WaypointNeighbourIterator` walks stay dead
//!   (the truth `ITER` lines all answer count 0);
//! - the chunk-level `AStar<ChunkWaypointState>` probes cross one portal
//!   at the exact logged crossing points (`XPATH` lines) and refuse two
//!   (`XPATHNONE` CB/AC — `crossedPortal_` blocks the second hop);
//! - the set-level `AStar<ChunkWPSetState>` probes walk the sets through
//!   the logged portal sequence (`SETHOP` lines) — the acceptance's
//!   「同图 Portal 访问顺序吻合」 — and refuse under the
//!   non-permissive block.

use foundation::math::Vector3;
use std::collections::HashMap;
use std::rc::Rc;
use waypoint::{
    AStar, ChunkWPSetState, ChunkWaypointSetData, ChunkWaypointState, ChunkWaypointVertexProvider,
    NavLoc, Portal, WaypointNeighbourIterator, find_by_girth, parse_world_navmesh,
};

const FIXTURE_DIR: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../docs/review/fixtures/waypoint"
);

fn truth_lines() -> Vec<String> {
    std::fs::read_to_string(format!("{FIXTURE_DIR}/gen_waypoints_cross.log"))
        .expect("truth log")
        .lines()
        .map(str::to_owned)
        .collect()
}

/// Address -> set index, the way the generator's `setIdxOf` numbered them.
fn index_sets(sets: &[Rc<ChunkWaypointSetData>]) -> HashMap<usize, usize> {
    sets.iter()
        .enumerate()
        .map(|(i, s)| (Rc::as_ptr(s) as *const () as usize, i))
        .collect()
}

fn idx_of(map: &HashMap<usize, usize>, set: &Rc<ChunkWaypointSetData>) -> usize {
    map[&(Rc::as_ptr(set) as *const () as usize)]
}

/// One `EDGELABEL` truth line: (set, absolute edge, neighbour set,
/// neighbour waypoint, entry point).
type LabelLine = (usize, u16, usize, i32, (f64, f64, f64));

/// The cross world after the wiring replay: the mounted sets, the four
/// portals and the address -> set index map.
type WiredWorld = (
    Vec<Rc<ChunkWaypointSetData>>,
    Vec<Rc<Portal>>,
    HashMap<usize, usize>,
);

/// The probe table row: a name plus the fixed start/end points (and the
/// non-permissive block for the set-level probes).
type Probe = (&'static str, (f64, f64, f64), (f64, f64, f64));
type SetProbe = (&'static str, (f64, f64, f64), (f64, f64, f64), i32);

/// The wired cross world: the decoded sets mounted on chunk identities
/// 0/1/2, plus the four portals, with the truth `EDGELABEL` lines replayed
/// through `connect()` in log order. The `CONNTO` lines fix which portal
/// each set pair rides and (like the reference `pPortal->pChunk`) which
/// chunk that portal points at.
fn cross_world() -> WiredWorld {
    let blob = std::fs::read(format!("{FIXTURE_DIR}/world_navmesh_cross.bin")).expect("fixture");
    let sets: Vec<Rc<_>> = parse_world_navmesh(&blob)
        .expect("the cross fixture decodes")
        .into_iter()
        .map(Rc::new)
        .collect();
    assert_eq!(sets.len(), 3, "three sets: A, B, C");
    for (id, set) in sets.iter().enumerate() {
        set.set_chunk_identity(id);
    }

    // The truth: portal flags, the (set, neighbour, portal) pairs, and the
    // per-edge labels.
    let mut portal_flags: Vec<(bool, bool)> = Vec::new();
    let mut conns: Vec<(usize, usize, usize)> = Vec::new();
    let mut labels: Vec<LabelLine> = Vec::new();
    for line in truth_lines() {
        let f: Vec<&str> = line.split_whitespace().collect();
        match f[0] {
            "PORTAL" => portal_flags.push((f[2] == "1", f[3] == "1")),
            "CONNTO" => conns.push((
                f[1].parse().expect("set"),
                f[3].parse().expect("nbr set"),
                f[4].parse().expect("portal"),
            )),
            "EDGELABEL" => labels.push((
                f[1].parse().expect("set"),
                f[2].parse().expect("edge"),
                f[3].parse().expect("nbr set"),
                f[4].parse().expect("nbr wp"),
                (
                    f[5].parse().expect("x"),
                    f[6].parse().expect("y"),
                    f[7].parse().expect("z"),
                ),
            )),
            _ => {}
        }
    }

    // Each portal serves exactly one ordered pair; `to_chunk` is the pair's
    // destination, mirroring `pPortal->pChunk`.
    let mut to_chunk: Vec<Option<usize>> = vec![None; portal_flags.len()];
    for (_set, nbr, portal) in &conns {
        to_chunk[*portal] = Some(*nbr);
    }
    let portals: Vec<Rc<Portal>> = to_chunk
        .into_iter()
        .zip(portal_flags)
        .map(|(to, (permissive, has_chunk_item))| {
            Rc::new(Portal {
                permissive,
                has_chunk_item,
                to_chunk: to,
            })
        })
        .collect();

    // Replay the wiring in truth order: one `connect()` per EDGELABEL line.
    for (set, edge, nbr, wp, (x, y, z)) in &labels {
        let portal = conns
            .iter()
            .find(|(s, n, _)| s == set && n == nbr)
            .map(|(_, _, p)| *p)
            .unwrap_or_else(|| panic!("no CONNTO for set {set} -> {nbr}"));
        sets[*set].connect(
            Rc::clone(&sets[*nbr]),
            *wp,
            Rc::clone(&portals[portal]),
            *edge,
            Vector3::new(*x as f32, *y as f32, *z as f32),
        );
    }
    let map = index_sets(&sets);
    (sets, portals, map)
}

/// The decoded `HDR`/`WP`/`EDGE` truth lines, checked against the port's
/// own reader (`readWaypointSet` replay).
#[test]
fn the_cross_fixture_decodes_to_the_truth_lines() {
    let blob = std::fs::read(format!("{FIXTURE_DIR}/world_navmesh_cross.bin")).expect("fixture");
    let sets: Vec<Rc<_>> = parse_world_navmesh(&blob)
        .expect("decode")
        .into_iter()
        .map(Rc::new)
        .collect();

    let mut total_edges = 0usize;
    let mut wp_seen = 0usize;
    for line in truth_lines() {
        let f: Vec<&str> = line.split_whitespace().collect();
        if f.is_empty() {
            continue;
        }
        if f[0].starts_with("HDR") && f[0] != "HDRX" {
            // HDR0 version 0 girth 0.600000 waypoints 1 edges 4
            let set: usize = f[0][3..].parse().expect("hdr set");
            assert_eq!(f[1], "version");
            assert_eq!(f[2], "0", "the old-graph record");
            assert_eq!(f[3], "girth");
            let girth: f64 = f[4].parse().expect("girth");
            let wps: usize = f[6].parse().expect("waypoints");
            let edges: usize = f[8].parse().expect("edges");
            assert!(
                (sets[set].girth() as f64 - girth).abs() <= 1e-6,
                "HDR {set}"
            );
            assert_eq!(sets[set].waypoints().len(), wps, "HDR {set}");
            total_edges += edges;
            continue;
        }
        if f[0] == "WP" {
            // WP <set> <wp> minh m0 maxh m1 edges e centre cx cz
            let (s, w): (usize, usize) = (f[1].parse().unwrap(), f[2].parse().unwrap());
            let wp = &sets[s].waypoints()[w];
            assert!((wp.min_height as f64 - f[4].parse::<f64>().unwrap()).abs() <= 1e-6);
            assert!((wp.max_height as f64 - f[6].parse::<f64>().unwrap()).abs() <= 1e-6);
            assert_eq!(wp.edge_count as usize, f[8].parse::<usize>().unwrap());
            assert!((wp.centre.x as f64 - f[10].parse::<f64>().unwrap()).abs() <= 1e-6);
            assert!((wp.centre.y as f64 - f[11].parse::<f64>().unwrap()).abs() <= 1e-6);
            wp_seen += 1;
            continue;
        }
        if f[0] == "EDGE" {
            // EDGE <set> <wp> <edge> <vtx> x y nbr(hex)
            let (s, w, e): (usize, usize, usize) = (
                f[1].parse().unwrap(),
                f[2].parse().unwrap(),
                f[3].parse().unwrap(),
            );
            let wp = &sets[s].waypoints()[w];
            let slot = wp.edge_range().start + e;
            let edge = &sets[s].edges()[slot];
            assert_eq!(
                edge.vertex_index,
                f[4].parse::<u16>().unwrap(),
                "EDGE {s}/{w}/{e} vertex index"
            );
            let vertex = sets[s].vertex_by_index(edge.vertex_index);
            assert!(
                (vertex.x as f64 - f[5].parse::<f64>().unwrap()).abs() <= 1e-6
                    && (vertex.y as f64 - f[6].parse::<f64>().unwrap()).abs() <= 1e-6,
                "EDGE {s}/{w}/{e} vertex coords"
            );
            let nbr = u16::from_str_radix(f[7], 16).expect("nbr hex");
            assert_eq!(edge.neighbour, nbr, "EDGE {s}/{w}/{e} neighbour");
        }
    }
    assert_eq!(wp_seen, 4, "A has 1, B has 2, C has 1");
    assert_eq!(total_edges, 16);
}

/// The `CONNTO`/`EDGELABEL` truth lines: the wiring `connect()` produced,
/// straight off the port's own containers, plus the backlinks the first
/// connection of each pair dropped.
#[test]
fn the_wiring_replays_the_conn_and_label_lines() {
    let (sets, portals, map) = cross_world();

    let mut conn_truth: Vec<(usize, Vec<(usize, usize)>)> = Vec::new();
    let mut label_truth: Vec<LabelLine> = Vec::new();
    for line in truth_lines() {
        let f: Vec<&str> = line.split_whitespace().collect();
        match f[0] {
            "CONNTO" => {
                let (s, seq, nbr, portal): (usize, usize, usize, usize) = (
                    f[1].parse().unwrap(),
                    f[2].parse().unwrap(),
                    f[3].parse().unwrap(),
                    f[4].parse().unwrap(),
                );
                if seq == 0 {
                    conn_truth.push((s, Vec::new()));
                }
                conn_truth
                    .last_mut()
                    .expect("CONNTO after its set opener")
                    .1
                    .push((nbr, portal));
            }
            "EDGELABEL" => label_truth.push((
                f[1].parse().unwrap(),
                f[2].parse().unwrap(),
                f[3].parse().unwrap(),
                f[4].parse().unwrap(),
                (
                    f[5].parse().unwrap(),
                    f[6].parse().unwrap(),
                    f[7].parse().unwrap(),
                ),
            )),
            _ => {}
        }
    }

    for (s, pairs) in &conn_truth {
        let mut got: Vec<(usize, usize)> = sets[*s]
            .connections()
            .iter()
            .map(|(set, portal)| {
                (
                    idx_of(&map, set),
                    portals
                        .iter()
                        .position(|p| Rc::ptr_eq(p, portal))
                        .expect("the stored portal is one of the four"),
                )
            })
            .collect();
        got.sort();
        let mut want = pairs.clone();
        want.sort();
        assert_eq!(got, want, "CONNTO set {s}");
    }

    for (s, edge, nbr, wp, (x, y, z)) in &label_truth {
        let label = sets[*s]
            .edge_label(*edge)
            .unwrap_or_else(|| panic!("EDGELABEL {s}/{edge} missing"));
        assert_eq!(idx_of(&map, &label.set), *nbr, "EDGELABEL {s}/{edge} set");
        assert_eq!(label.waypoint, *wp, "EDGELABEL {s}/{edge} waypoint");
        assert!(
            (label.entry_point.x as f64 - x).abs() <= 1e-6
                && (label.entry_point.y as f64 - y).abs() <= 1e-6
                && (label.entry_point.z as f64 - z).abs() <= 1e-6,
            "EDGELABEL {s}/{edge} entry ({:.6},{:.6},{:.6})",
            label.entry_point.x,
            label.entry_point.y,
            label.entry_point.z
        );
    }

    // Backlinks: the first connection of each pair dropped one on the
    // target. a:[B], b:[A,C], c:[B].
    let mut backlink_truth: Vec<Vec<usize>> = vec![Vec::new(); sets.len()];
    for (s, pairs) in &conn_truth {
        for (nbr, _) in pairs {
            backlink_truth[*nbr].push(*s);
        }
    }
    for (s, want) in backlink_truth.iter().enumerate() {
        let mut got: Vec<usize> = sets[s]
            .backlinks()
            .iter()
            .map(|b| idx_of(&map, b))
            .collect();
        got.sort();
        let mut want = want.clone();
        want.sort();
        assert_eq!(got, want, "backlinks of set {s}");
    }
}

/// The `ITER <set> <wp> (count 0)` truth lines: the freshly constructed
/// `WaypointNeighbourIterator` is already ended, because the cursor guard
/// tests `adjacentRangeEnd` before the increment.
#[test]
fn the_neighbour_iterator_walks_stay_dead() {
    let (sets, _, _) = cross_world();
    let mut iter_truth: Vec<(usize, usize, usize)> = Vec::new();
    for line in truth_lines() {
        let f: Vec<&str> = line.split_whitespace().collect();
        if f[0] == "ITER" {
            // ITER <set> <wp> (count <n>) — the count token carries the
            // closing parenthesis.
            iter_truth.push((
                f[1].parse().unwrap(),
                f[2].parse().unwrap(),
                f[4].trim_end_matches(')').parse().unwrap(),
            ));
        }
    }
    assert_eq!(iter_truth.len(), 4, "A1 + B2 + C1 walks");
    for (s, wp, count) in iter_truth {
        let mut iterator = WaypointNeighbourIterator::new(&sets[s], wp as i32);
        let mut walked = 0;
        while !iterator.ended() {
            walked += 1;
            iterator.advance();
            assert!(walked < 10, "the walk must stay dead");
        }
        assert_eq!(walked, count, "ITER {s}/{wp}");
    }
}

/// The `XPATH` truth lines: the chunk-level `AStar<ChunkWaypointState>`
/// over the wired world. AB/BA/BC cross their portal at the logged point;
/// CB/AC answer none because `crossedPortal_` blocks the second hop; AA
/// is a single state.
#[test]
fn the_chunk_path_probes_cross_where_the_truth_says() {
    // The probe table, the same constants as the generator's block.
    let probes: [Probe; 6] = [
        ("AB", (0.15, 0.4, 0.4), (0.50, 0.4, 0.25)),
        ("BA", (0.50, 0.4, 0.25), (0.15, 0.4, 0.4)),
        ("BC", (0.50, 0.4, 0.25), (0.85, 0.4, 0.5)),
        ("CB", (0.85, 0.4, 0.5), (0.50, 0.4, 0.75)),
        ("AC", (0.15, 0.4, 0.4), (0.85, 0.4, 0.5)),
        ("AA", (0.15, 0.4, 0.4), (0.20, 0.4, 0.6)),
    ];

    enum Step {
        At(f64, f64, f64, i32, usize),
        None,
    }
    let mut truth: Vec<(String, i32, i32, Vec<Step>)> = Vec::new();
    for line in truth_lines() {
        let f: Vec<&str> = line.split_whitespace().collect();
        match f[0] {
            "XPATHBEGIN" => truth.push((
                f[1].to_owned(),
                f[2].parse().unwrap(),
                f[3].parse().unwrap(),
                Vec::new(),
            )),
            "XPATH" => truth
                .last_mut()
                .expect("XPATH after XPATHBEGIN")
                .3
                .push(Step::At(
                    f[2].parse().unwrap(),
                    f[3].parse().unwrap(),
                    f[4].parse().unwrap(),
                    f[5].parse().unwrap(),
                    f[6].parse().unwrap(),
                )),
            "XPATHNONE" => truth
                .last_mut()
                .expect("XPATHNONE after XPATHBEGIN")
                .3
                .push(Step::None),
            _ => {}
        }
    }
    assert_eq!(truth.len(), probes.len(), "every probe has truth lines");

    let (sets, _, map) = cross_world();

    // The chunk the probe point sits in, the generator's box test.
    let chunk_of = |x: f64| -> usize {
        if x < 1.0 / 3.0 {
            0
        } else if x < 2.0 / 3.0 {
            1
        } else {
            2
        }
    };

    for (name, src_at, dst_at) in probes {
        let truth = truth.iter().find(|(n, ..)| n == name).expect("probe");
        let (src_wp_truth, dst_wp_truth) = (truth.1, truth.2);

        // The reference resolved the waypoints through the chunk-level
        // find constructor; the port resolves through `find_by_girth` and
        // must land on the same waypoints.
        let find_at = |at: (f64, f64, f64)| {
            find_by_girth(
                &sets,
                Vector3::new(at.0 as f32, at.1 as f32, at.2 as f32),
                0.6,
                false,
            )
        };
        let src_wp = find_at(src_at).waypoint();
        let dst_wp = find_at(dst_at).waypoint();
        assert_eq!(src_wp, src_wp_truth, "XPATHBEGIN {name} src");
        assert_eq!(dst_wp, dst_wp_truth, "XPATHBEGIN {name} dst");

        // The find constructor lifts the point by 0.01 before the search.
        let src_state = ChunkWaypointState::from_navloc(&NavLoc::from_parts(
            Rc::clone(&sets[chunk_of(src_at.0)]),
            src_wp,
            Vector3::new(src_at.0 as f32, (src_at.1 + 0.01) as f32, src_at.2 as f32),
        ));
        let dst_state = ChunkWaypointState::from_navloc(&NavLoc::from_parts(
            Rc::clone(&sets[chunk_of(dst_at.0)]),
            dst_wp,
            Vector3::new(dst_at.0 as f32, (dst_at.1 + 0.01) as f32, dst_at.2 as f32),
        ));

        let mut astar = AStar::default();
        let found = astar.search(&src_state, &dst_state, -1.0);

        match truth.3.first() {
            Some(Step::None) => {
                assert!(!found, "XPATHNONE {name}");
            }
            Some(Step::At(..)) => {
                assert!(found, "XPATH {name}");
                let path = astar.path();
                assert_eq!(path.len(), truth.3.len(), "path length {name}");
                for (i, step) in truth.3.iter().enumerate() {
                    let Step::At(x, y, z, wp, set) = step else {
                        unreachable!()
                    };
                    let loc = path[i].nav_loc();
                    let p = loc.point();
                    assert!(
                        (p.x as f64 - x).abs() <= 1e-6
                            && (p.y as f64 - y).abs() <= 1e-6
                            && (p.z as f64 - z).abs() <= 1e-6,
                        "XPATH {name} step {i} point ({:.6},{:.6},{:.6}) vs ({x},{y},{z})",
                        p.x,
                        p.y,
                        p.z
                    );
                    assert_eq!(loc.waypoint(), *wp, "XPATH {name} step {i} waypoint");
                    let set_of = loc.p_set().expect("a located set");
                    assert_eq!(idx_of(&map, set_of), *set, "XPATH {name} step {i} set");
                }
            }
            None => panic!("probe {name} has no truth steps"),
        }
    }
}

/// The `SETSEARCH`/`SETHOP` truth lines: the set-level
/// `AStar<ChunkWPSetState>` over the connections. The `SETHOP` hop column
/// is the portal identity each hop rides — the acceptance's
/// 「同图 Portal 访问顺序吻合」.
#[test]
fn the_set_search_walks_the_portals_in_the_truth_order() {
    // The probe table, the same constants as the generator's block.
    let probes: [SetProbe; 5] = [
        ("AC0", (0.15, 0.4, 0.4), (0.85, 0.4, 0.5), 0),
        ("AC1", (0.15, 0.4, 0.4), (0.85, 0.4, 0.5), 1),
        ("CA0", (0.85, 0.4, 0.6), (0.15, 0.4, 0.4), 0),
        ("AB1", (0.15, 0.4, 0.4), (0.50, 0.4, 0.25), 1),
        ("BC0", (0.50, 0.4, 0.25), (0.85, 0.4, 0.5), 0),
    ];

    #[derive(Clone)]
    enum Hop {
        At(f64, f64, f64, usize, bool, bool, i32),
        None,
    }
    let mut truth: Vec<(String, i32, usize, usize, Vec<Hop>)> = Vec::new();
    for line in truth_lines() {
        let f: Vec<&str> = line.split_whitespace().collect();
        match f[0] {
            "SETSEARCHBEGIN" => truth.push((
                f[1].to_owned(),
                f[2].parse().unwrap(),
                f[3].parse().unwrap(),
                f[4].parse().unwrap(),
                Vec::new(),
            )),
            // SETHOP <name> <seq> <set> <x> <y> <z> <shell> <activated> <portal>
            "SETHOP" => truth
                .last_mut()
                .expect("SETHOP after SETSEARCHBEGIN")
                .4
                .push(Hop::At(
                    f[4].parse().unwrap(),
                    f[5].parse().unwrap(),
                    f[6].parse().unwrap(),
                    f[3].parse().unwrap(),
                    f[7] == "1",
                    f[8] == "1",
                    f[9].parse().unwrap(),
                )),
            "SETSEARCHNONE" => truth
                .last_mut()
                .expect("SETSEARCHNONE after SETSEARCHBEGIN")
                .4
                .push(Hop::None),
            _ => {}
        }
    }
    assert_eq!(truth.len(), probes.len(), "every set probe has truth");

    let (sets, portals, map) = cross_world();

    for (name, src_at, dst_at, block) in probes {
        let (_, block_truth, src_set_truth, dst_set_truth, hops) =
            truth.iter().find(|(n, ..)| n == name).expect("probe");
        assert_eq!(block, *block_truth, "SETSEARCHBEGIN {name} block");

        let find_set_at = |at: (f64, f64, f64)| {
            find_by_girth(
                &sets,
                Vector3::new(at.0 as f32, at.1 as f32, at.2 as f32),
                0.6,
                false,
            )
            .p_set()
            .expect("a set")
            .clone()
        };
        let src_set = find_set_at(src_at);
        let dst_set = find_set_at(dst_at);
        assert_eq!(idx_of(&map, &src_set), *src_set_truth, "{name} src set");
        assert_eq!(idx_of(&map, &dst_set), *dst_set_truth, "{name} dst set");

        let mut src_state = ChunkWPSetState::from_navloc(&NavLoc::from_parts(
            Rc::clone(&src_set),
            0,
            Vector3::new(src_at.0 as f32, (src_at.1 + 0.01) as f32, src_at.2 as f32),
        ));
        src_state.set_block_non_permissive(*block_truth != 0);
        let dst_state = ChunkWPSetState::from_navloc(&NavLoc::from_parts(
            Rc::clone(&dst_set),
            0,
            Vector3::new(dst_at.0 as f32, (dst_at.1 + 0.01) as f32, dst_at.2 as f32),
        ));

        let mut astar = AStar::<ChunkWPSetState>::default();
        let found = astar.search(&src_state, &dst_state, -1.0);

        match hops.first() {
            Some(Hop::None) => {
                assert!(!found, "SETSEARCHNONE {name}");
            }
            Some(Hop::At(..)) => {
                assert!(found, "SETSEARCH {name}");
                let path = astar.path();
                assert_eq!(path.len(), hops.len(), "hop count {name}");
                for (seq, hop) in hops.iter().enumerate() {
                    let Hop::At(x, y, z, set, shell, activated, portal_truth) = hop else {
                        unreachable!()
                    };
                    let state = &path[seq];
                    let p = state.position();
                    assert!(
                        (p.x as f64 - x).abs() <= 1e-6
                            && (p.y as f64 - y).abs() <= 1e-6
                            && (p.z as f64 - z).abs() <= 1e-6,
                        "SETHOP {name} {seq} position ({:.6},{:.6},{:.6}) vs ({x},{y},{z})",
                        p.x,
                        p.y,
                        p.z
                    );
                    let set_of = state.p_set().expect("a set state");
                    assert_eq!(idx_of(&map, set_of), *set, "SETHOP {name} {seq} set");
                    assert_eq!(
                        state.passed_shell_boundary(),
                        *shell,
                        "SETHOP {name} {seq} shell"
                    );
                    assert_eq!(
                        state.passed_activated_portal(),
                        *activated,
                        "SETHOP {name} {seq} activated"
                    );

                    // The portal identity: the previous state's stored
                    // connection to this set, resolved back to the portal
                    // index like the generator's `portalIdxOf`.
                    let hop_portal = if seq == 0 {
                        -1
                    } else {
                        let prev = &path[seq - 1];
                        match prev.p_set().expect("prev set").connection_portal(set_of) {
                            Some(portal) => portals
                                .iter()
                                .position(|p| Rc::ptr_eq(p, &portal))
                                .expect("stored portal is one of the four")
                                as i32,
                            None => -1,
                        }
                    };
                    assert_eq!(hop_portal, *portal_truth, "SETHOP {name} {seq} portal");
                }
            }
            None => panic!("probe {name} has no truth hops"),
        }
    }
}
