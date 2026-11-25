//! BW-10-07-03 acceptance: the generated two-chunk map loads through
//! the server face — the assets decode into one set per own chunk's
//! navigator, and the original `ChunkWaypointSet::bind()` walk wires
//! the seam's another-chunk edges through the permissive portal pair
//! (`gen_mapload.log`). After the bind, the same chunk-level and
//! set-level A* probes the reference answered replay identically —
//! including the seam crossing both ways and the one-crossing-rule
//! refusal for the long tour.

use foundation::math::{BoundBox, PlaneEq, Vector3};
use std::rc::Rc;
use waypoint::{
    AStar, BindChunk, BindPortal, BindSpace, ChunkWPSetState, ChunkWaypointSetData,
    ChunkWaypointState, NavLoc, Portal, WaypointNeighbourIterator, find_by_girth,
    parse_world_navmesh,
};

type OwnedSet = Rc<ChunkWaypointSetData>;

const FIXTURE_DIR: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../docs/review/fixtures/waypoint"
);

const GIRTH: f32 = 0.6;

/// The harness's `NavLoc::from_parts` stand-in girth (the searches never
/// read it; the sets carry the real 0.6).
const STATE_GIRTH: f32 = 100.0;

fn asset(name: &str) -> Vec<u8> {
    std::fs::read(format!("{FIXTURE_DIR}/{name}")).unwrap_or_else(|e| panic!("read {name}: {e}"))
}

fn truth_lines() -> Vec<String> {
    std::fs::read_to_string(format!("{FIXTURE_DIR}/gen_mapload.log"))
        .expect("the gen_mapload truth log")
        .lines()
        .map(str::to_owned)
        .collect()
}

/// The two decoded sets: index 0 is mapA (chunk A), index 1 is mapB
/// (chunk B) — the harness's `setIdxOf` answers by the mount order.
fn decoded_sets() -> [OwnedSet; 2] {
    let a = parse_world_navmesh(&asset("world_navmesh_genmap_a.bin")).expect("mapA decodes");
    let b = parse_world_navmesh(&asset("world_navmesh_genmap_b.bin")).expect("mapB decodes");
    let mut a = a;
    let mut b = b;
    assert_eq!(a.len(), 1, "mapA records");
    assert_eq!(b.len(), 1, "mapB records");
    [Rc::new(a.remove(0)), Rc::new(b.remove(0))]
}

fn set_idx(set: &OwnedSet, sets: &[OwnedSet; 2]) -> usize {
    if Rc::ptr_eq(set, &sets[0]) {
        0
    } else {
        assert!(Rc::ptr_eq(set, &sets[1]), "foreign set");
        1
    }
}

/// The harness's portal pair: permissive both ways, the same rect on
/// the seam, and the bind projection's axes (the identity chunks keep x
/// and z; the height lands mid-band at 4.1). `findBetterPortal` reads
/// the rect corners as 2D (u, v) — the harness bakes them into the
/// Vector3's x and y slots.
fn portal_flags() -> [Rc<Portal>; 2] {
    [
        Rc::new(Portal {
            permissive: true,
            has_chunk_item: false,
        }),
        Rc::new(Portal {
            permissive: true,
            has_chunk_item: false,
        }),
    ]
}

fn portal_on_seam(far: usize, flags: &Rc<Portal>) -> BindPortal {
    BindPortal {
        plane: PlaneEq::new(Vector3::new(1.0, 0.0, 0.0), 5.0),
        points: vec![(4.4, 9.9), (4.4, 0.1), (5.6, 0.1), (5.6, 9.9)],
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

/// The two-chunk `BindSpace` off the truth `CHUNK` lines: inside
/// chunks, loaded, one set per chunk navigator, the permissive portal
/// pair pointing at each other.
fn bind_space(sets: &[OwnedSet; 2], flags: &[Rc<Portal>; 2]) -> BindSpace {
    BindSpace {
        chunks: vec![
            BindChunk {
                bb: BoundBox::new(Vector3::new(0.0, 0.0, 0.0), Vector3::new(5.0, 15.0, 10.0)),
                outside: false,
                loaded: true,
                overlappers_loaded: false,
                portals: vec![portal_on_seam(1, &flags[0])],
                sets: vec![Rc::clone(&sets[0])],
            },
            BindChunk {
                bb: BoundBox::new(Vector3::new(5.0, 0.0, 0.0), Vector3::new(10.0, 15.0, 10.0)),
                outside: false,
                loaded: true,
                overlappers_loaded: false,
                portals: vec![portal_on_seam(0, &flags[1])],
                sets: vec![Rc::clone(&sets[1])],
            },
        ],
    }
}

/// The bound world: the decoded sets with the neighbour-edge collection
/// and the original `bind()` walk run on both chunks, exactly the
/// truth log's order (A first, then B).
fn bound_world() -> ([OwnedSet; 2], [Rc<Portal>; 2]) {
    let sets = decoded_sets();
    let flags = portal_flags();
    let space = bind_space(&sets, &flags);
    // the navigator mount (`ChunkNavigator::instance(*pChunk).add(...)`)
    // pins each set to its chunk; the crossing arm refuses a
    // destination set with no mounted chunk
    sets[0].set_chunk_identity(0);
    sets[1].set_chunk_identity(1);
    sets[0].build_neighbour_edges();
    sets[1].build_neighbour_edges();
    sets[0].bind(&space, 0);
    sets[1].bind(&space, 1);
    (sets, flags)
}

/// The reference `NavLoc(Chunk*, point, girth)` resolution asked of the
/// chunk the point belongs to: the lift, then the navigator's find over
/// that chunk's mounted sets (containment first, the closest fallback
/// second — `find_by_girth` is that two-phase face).
fn resolve(chunk: usize, sets: &[OwnedSet; 2], at: Vector3) -> (OwnedSet, i32) {
    let point = Vector3::new(at.x, at.y + 0.01, at.z);
    let res = find_by_girth(&[Rc::clone(&sets[chunk])], point, GIRTH, false);
    let set = res.p_set().expect("the chunk's set answers").clone();
    (set, res.waypoint())
}

fn close(a: f64, truth: f64) -> bool {
    // The truth prints %.6f.
    (a - truth).abs() <= 1e-5 * truth.abs().max(1.0)
}

fn point_close(p: Vector3, x: f64, y: f64, z: f64) -> bool {
    (p.x as f64 - x).abs() <= 1e-6
        && (p.y as f64 - y).abs() <= 1e-6
        && (p.z as f64 - z).abs() <= 1e-6
}

// ---- the truth probes the harness hardcodes (the XPATHBEGIN /
// SETSEARCHBEGIN lines carry only the waypoint ids) ----

/// `(name, srcChunk, src, dstChunk, dst)` of the chunk-level probes.
const XPATH_PROBES: [(&str, usize, Vector3, usize, Vector3); 6] = [
    // across chunk A west->east: the hole forces the ramp detour
    (
        "ACROSS",
        0,
        Vector3::new(1.0, 4.0, 1.0),
        0,
        Vector3::new(4.0, 4.0, 1.0),
    ),
    // west ground up to the ramp top band
    (
        "RAMP",
        0,
        Vector3::new(1.0, 4.0, 1.0),
        0,
        Vector3::new(2.0, 8.0, 8.0),
    ),
    // across chunk B south->north around the obstacle absence
    (
        "AROUND",
        1,
        Vector3::new(6.0, 4.0, 2.0),
        1,
        Vector3::new(6.0, 4.0, 8.0),
    ),
    // over the seam through the portal wiring
    (
        "SEAM",
        0,
        Vector3::new(1.0, 4.0, 1.0),
        1,
        Vector3::new(6.0, 4.0, 2.0),
    ),
    // ... and back: getAdjacency refuses to expand a state that just
    // crossed a portal, so the goal must be the crossing target (A wp3,
    // where B's wired edge lands)
    (
        "SEAMBA",
        1,
        Vector3::new(6.0, 4.0, 2.0),
        0,
        Vector3::new(4.0, 4.0, 1.0),
    ),
    // the long tour wants to KEEP GOING after the crossing — the
    // one-crossing rule refuses; XPATHNONE is the reference's answer
    (
        "TOUR",
        0,
        Vector3::new(1.0, 4.0, 1.0),
        1,
        Vector3::new(6.0, 4.0, 8.0),
    ),
];

/// `(name, srcChunk, src, dstChunk, dst, blockNonPermissive)` of the
/// set-level probes.
const SET_PROBES: [(&str, usize, Vector3, usize, Vector3, bool); 5] = [
    // the permissive seam answers regardless of the block flag
    (
        "SEAM",
        0,
        Vector3::new(1.0, 4.0, 1.0),
        1,
        Vector3::new(6.0, 4.0, 2.0),
        false,
    ),
    (
        "SEAMB",
        0,
        Vector3::new(1.0, 4.0, 1.0),
        1,
        Vector3::new(6.0, 4.0, 2.0),
        true,
    ),
    (
        "SEAMBA",
        1,
        Vector3::new(6.0, 4.0, 2.0),
        0,
        Vector3::new(1.0, 4.0, 1.0),
        false,
    ),
    // same-set sanity walks
    (
        "WITHINA",
        0,
        Vector3::new(1.0, 4.0, 1.0),
        0,
        Vector3::new(4.0, 4.0, 1.0),
        false,
    ),
    (
        "WITHINB",
        1,
        Vector3::new(6.0, 4.0, 2.0),
        1,
        Vector3::new(6.0, 4.0, 8.0),
        false,
    ),
];

/// The portal index a connection rode: the harness's `portalIdxOf` by
/// pointer identity against the mounted pair.
fn portal_idx(portal: &Rc<Portal>, flags: &[Rc<Portal>; 2]) -> usize {
    if Rc::ptr_eq(portal, &flags[0]) {
        0
    } else {
        assert!(Rc::ptr_eq(portal, &flags[1]), "foreign portal");
        1
    }
}

#[test]
fn the_loaded_map_binds_through_the_permissive_portal_pair() {
    let lines = truth_lines();
    let sets = decoded_sets();

    // LHDR: the decoded record headers.
    for line in &lines {
        let f: Vec<&str> = line.split_whitespace().collect();
        if f[0] != "LHDR" {
            continue;
        }
        let set = &sets[if f[1] == "mapA" { 0 } else { 1 }];
        assert_eq!(f[4], "0", "{} version", f[1]);
        assert!(
            close(set.girth() as f64, f[6].parse().unwrap()),
            "{} girth",
            f[1]
        );
        assert_eq!(set.waypoints().len(), f[8].parse::<usize>().unwrap());
        assert_eq!(set.edges().len(), f[10].parse::<usize>().unwrap());
    }

    let flags = portal_flags();
    let space = bind_space(&sets, &flags);
    sets[0].build_neighbour_edges();
    sets[1].build_neighbour_edges();
    sets[0].bind(&space, 0);
    sets[1].bind(&space, 1);

    // CONNTO: one connection per chunk, to the other set through the
    // own portal.
    let conns = sets[0].connections();
    assert_eq!(conns.len(), 1, "chunk A connections");
    assert_eq!(set_idx(&conns[0].0, &sets), 1, "CONNTO 0 0 1 0");
    assert_eq!(portal_idx(&conns[0].1, &flags), 0, "CONNTO 0 0 1 0");
    let conns = sets[1].connections();
    assert_eq!(conns.len(), 1, "chunk B connections");
    assert_eq!(set_idx(&conns[0].0, &sets), 0, "CONNTO 1 0 0 1");
    assert_eq!(portal_idx(&conns[0].1, &flags), 1, "CONNTO 1 0 0 1");

    // EDGELABEL: the truth's five wired edges, entry = the probe point
    // (the edge midpoint, the working probe height).
    for line in &lines {
        let f: Vec<&str> = line.split_whitespace().collect();
        if f[0] != "EDGELABEL" {
            continue;
        }
        let chunk: usize = f[1].parse().unwrap();
        let edge: u16 = f[2].parse().unwrap();
        let want_set: usize = f[3].parse().unwrap();
        let want_wp: i32 = f[4].parse().unwrap();
        let x: f64 = f[5].parse().unwrap();
        let y: f64 = f[6].parse().unwrap();
        let z: f64 = f[7].parse().unwrap();

        let label = sets[chunk]
            .edge_label(edge)
            .unwrap_or_else(|| panic!("EDGELABEL {chunk} {edge} missing"));
        assert_eq!(
            set_idx(&label.set, &sets),
            want_set,
            "EDGELABEL {chunk} {edge} set"
        );
        assert_eq!(label.waypoint, want_wp, "EDGELABEL {chunk} {edge} wp");
        assert!(
            point_close(label.entry_point, x, y, z),
            "EDGELABEL {chunk} {edge} entry ({},{},{}) vs ({x},{y},{z})",
            label.entry_point.x,
            label.entry_point.y,
            label.entry_point.z
        );
    }
    let keys_a: Vec<u16> = sets[0]
        .edge_labels()
        .into_iter()
        .map(|(edge, _)| edge)
        .collect();
    assert_eq!(keys_a, [16, 23], "chunk A wired exactly edges 16 and 23");
    let keys_b: Vec<u16> = sets[1]
        .edge_labels()
        .into_iter()
        .map(|(edge, _)| edge)
        .collect();
    assert_eq!(
        keys_b,
        [18, 21, 30],
        "chunk B wired exactly edges 18, 21, 30"
    );

    // ITER: the born-dead neighbour walks — the reference iterator's
    // guard order ends a freshly built iterator before the first step,
    // so every waypoint yields nothing even after the bind.
    for (chunk, set) in sets.iter().enumerate() {
        for i in 0..set.waypoints().len() {
            let mut count = 0usize;
            let mut wni = WaypointNeighbourIterator::new(set, i as i32);
            while !wni.ended() {
                count += 1;
                wni.advance();
            }
            assert_eq!(count, 0, "ITER {chunk} {i} (count 0)");
        }
    }
}

#[test]
fn the_bound_map_answers_the_reference_path_probes() {
    let lines = truth_lines();
    let (sets, flags) = bound_world();

    // NAVLOC: where the resolver drops each probe point, asked of the
    // chunk the point belongs to.
    let mut navlocs = 0usize;
    for line in &lines {
        let f: Vec<&str> = line.split_whitespace().collect();
        if f[0] != "NAVLOC" {
            continue;
        }
        navlocs += 1;
        let chunk: usize = f[2].parse().unwrap();
        let at = Vector3::new(
            f[3].parse().unwrap(),
            f[4].parse().unwrap(),
            f[5].parse().unwrap(),
        );
        let want_wp: i32 = f[7].parse().unwrap();
        let want_set: usize = f[9].parse().unwrap();
        let want_y: f64 = f[11].parse().unwrap();

        let (set, wp) = resolve(chunk, &sets, at);
        assert_eq!(wp, want_wp, "NAVLOC {} waypoint", f[1]);
        assert_eq!(set_idx(&set, &sets), want_set, "NAVLOC {} set", f[1]);
        assert!(close(at.y as f64 + 0.01, want_y), "NAVLOC {} y", f[1]);
    }
    assert_eq!(navlocs, 9, "every NAVLOC probe replayed");

    // XPATH: the chunk-level A* over the bound graph.
    let mut paths = 0usize;
    let mut nones = 0usize;
    for (name, src_chunk, src, dst_chunk, dst) in XPATH_PROBES {
        let (src_set, src_wp) = resolve(src_chunk, &sets, src);
        let (dst_set, dst_wp) = resolve(dst_chunk, &sets, dst);

        let mut begin = None;
        let mut steps = Vec::new();
        let mut none = false;
        for line in &lines {
            let f: Vec<&str> = line.split_whitespace().collect();
            if f.len() < 2 || f[1] != name {
                continue;
            }
            match f[0] {
                "XPATHBEGIN" => begin = Some((f[2].parse::<i32>().unwrap(), f[3].parse().unwrap())),
                "XPATH" => steps.push((
                    f[2].parse::<f64>().unwrap(),
                    f[3].parse().unwrap(),
                    f[4].parse().unwrap(),
                    f[5].parse::<i32>().unwrap(),
                    f[6].parse::<usize>().unwrap(),
                )),
                "XPATHNONE" => none = true,
                _ => {}
            }
        }
        let (want_src, want_dst) = begin.unwrap_or_else(|| panic!("XPATHBEGIN {name}"));
        assert_eq!(src_wp, want_src, "XPATHBEGIN {name} src wp");
        assert_eq!(dst_wp, want_dst, "XPATHBEGIN {name} dst wp");

        let src_state = ChunkWaypointState::from_navloc(&NavLoc::from_parts(
            src_set,
            src_wp,
            Vector3::new(src.x, src.y + 0.01, src.z),
            STATE_GIRTH,
        ));
        let dst_state = ChunkWaypointState::from_navloc(&NavLoc::from_parts(
            dst_set,
            dst_wp,
            Vector3::new(dst.x, dst.y + 0.01, dst.z),
            STATE_GIRTH,
        ));
        let mut astar = AStar::default();
        let found = astar.search(&src_state, &dst_state, -1.0);

        if none {
            assert!(!found, "XPATHNONE {name} — the port's search must fail too");
            nones += 1;
            continue;
        }
        assert!(found, "XPATH {name}");
        paths += 1;
        let steps_port = astar.path();
        assert_eq!(steps_port.len(), steps.len(), "XPATH {name} length");
        for (i, want) in steps.iter().enumerate() {
            let loc = steps_port[i].nav_loc();
            assert_eq!(loc.waypoint(), want.3, "XPATH {name} step {i} wp");
            assert_eq!(
                set_idx(loc.p_set().unwrap(), &sets),
                want.4,
                "XPATH {name} step {i} set"
            );
            let p = loc.point();
            assert!(
                point_close(p, want.0, want.1, want.2),
                "XPATH {name} step {i} point ({},{},{}) vs ({},{},{})",
                p.x,
                p.y,
                p.z,
                want.0,
                want.1,
                want.2
            );
        }
    }
    assert_eq!(paths, 5, "ACROSS/RAMP/AROUND/SEAM/SEAMBA path");
    assert_eq!(nones, 1, "TOUR stays beyond the one-crossing rule");

    // SETSEARCH: the set-level A* over the connections, with the SETHOP
    // chain's portal identity.
    for (name, src_chunk, src, dst_chunk, dst, block) in SET_PROBES {
        let (src_set, src_wp) = resolve(src_chunk, &sets, src);
        let (dst_set, dst_wp) = resolve(dst_chunk, &sets, dst);
        let mut src_state = ChunkWPSetState::from_navloc(&NavLoc::from_parts(
            Rc::clone(&src_set),
            src_wp,
            Vector3::new(src.x, src.y + 0.01, src.z),
            STATE_GIRTH,
        ));
        let dst_state = ChunkWPSetState::from_navloc(&NavLoc::from_parts(
            Rc::clone(&dst_set),
            dst_wp,
            Vector3::new(dst.x, dst.y + 0.01, dst.z),
            STATE_GIRTH,
        ));
        src_state.set_block_non_permissive(block);

        // SETSEARCHBEGIN name block srcSet dstSet.
        for line in &lines {
            let f: Vec<&str> = line.split_whitespace().collect();
            if f[0] == "SETSEARCHBEGIN" && f[1] == name {
                assert_eq!(
                    f[2].parse::<i32>().unwrap(),
                    i32::from(block),
                    "{name} block"
                );
                assert_eq!(
                    f[3].parse::<usize>().unwrap(),
                    set_idx(&src_set, &sets),
                    "{name} srcSet"
                );
                assert_eq!(
                    f[4].parse::<usize>().unwrap(),
                    set_idx(&dst_set, &sets),
                    "{name} dstSet"
                );
            }
        }

        let mut astar = AStar::default();
        let found = astar.search(&src_state, &dst_state, -1.0);
        let hops: Vec<Vec<String>> = lines
            .iter()
            .filter_map(|line| {
                let f: Vec<&str> = line.split_whitespace().collect();
                (f[0] == "SETHOP" && f[1] == name)
                    .then(|| f.iter().map(|s| s.to_string()).collect())
            })
            .collect();
        assert!(found, "SETSEARCH {name}");
        let path = astar.path();
        assert_eq!(path.len(), hops.len(), "SETSEARCH {name} hop count");

        for (seq, state) in path.iter().enumerate() {
            let f = &hops[seq];
            let want_seq: usize = f[2].parse().unwrap();
            let want_set: usize = f[3].parse().unwrap();
            let x: f64 = f[4].parse().unwrap();
            let y: f64 = f[5].parse().unwrap();
            let z: f64 = f[6].parse().unwrap();
            let shell: bool = f[7].parse::<i32>().unwrap() != 0;
            let activated: bool = f[8].parse::<i32>().unwrap() != 0;
            let hop: i32 = f[9].parse().unwrap();

            assert_eq!(want_seq, seq, "SETHOP {name} seq");
            assert_eq!(
                set_idx(state.p_set().unwrap(), &sets),
                want_set,
                "SETHOP {name} {seq} set"
            );
            let p = state.position();
            assert!(
                point_close(p, x, y, z),
                "SETHOP {name} {seq} position ({},{},{}) vs ({x},{y},{z})",
                p.x,
                p.y,
                p.z
            );
            assert_eq!(
                state.passed_shell_boundary(),
                shell,
                "SETHOP {name} {seq} shell"
            );
            assert_eq!(
                state.passed_activated_portal(),
                activated,
                "SETHOP {name} {seq} activated"
            );
            if seq == 0 {
                assert_eq!(hop, -1, "SETHOP {name} 0 hop");
            } else {
                let prev = path[seq - 1].p_set().unwrap();
                let portal = prev.connection_portal(state.p_set().unwrap()).unwrap();
                assert_eq!(
                    portal_idx(&portal, &flags) as i32,
                    hop,
                    "SETHOP {name} {seq} hop portal"
                );
            }
        }
    }
}
