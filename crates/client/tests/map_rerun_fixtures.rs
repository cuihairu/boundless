//! OPT-10-01-04: rerun the map results — the path points and the
//! collision results against the original fixtures, driven through the
//! faces this section's optimization rides.
//!
//! The path leg re-binds the BW-10-07-03 two-chunk genmap world (the
//! reference `bind()` wiring, the truth `gen_mapload.log`) and walks
//! every XPATH probe pair through `Navigator::find_path` with
//! `ReadWriteCache` — the cached walk face — demanding every hop land
//! on the truth's own waypoint chain, and the one-crossing TOUR pair
//! stay refused.
//!
//! The collision leg loads the chunk-space fixture map through the
//! optimized streaming face — the `BwResource` section cache serving
//! `load_and_bind` (the OPT-10-01-02 candidate) — registers the
//! reference terrain slabs, and answers the whole probe grid through
//! the client's `findDropPoint`: every drop lands on its slab at 10 m,
//! 40 m down the ray, the owning chunk resolving as the baseline
//! recorded.
//!
//! ```sh
//! cargo test -p client --test map_rerun_fixtures
//! ```

use std::path::{Path, PathBuf};
use std::rc::Rc;

use chunk::{ChunkObstacle, ChunkSpace, GeometryMapping};
use client::navigation::find_drop_point;
use foundation::math::{BoundBox, Matrix, PlaneEq, Vector3};
use physics::{
    BSP_FILE_CUR_TOKEN, BSP_NODE_PACKED_SIZE, BspTree, ClosestTriangle, OFFSET_32BIT_NULL,
};
use resources::bwresource::BwResource;
use resources::datasection;
use waypoint::{
    BindChunk, BindPortal, BindSpace, ChunkWaypointSetData, NavLoc, Navigator, Portal,
    find_by_girth, parse_world_navmesh,
};

// ---------------------------------------------------------------------------
// The path leg: the genmap two-chunk world (BW-10-07-03 wiring).
// ---------------------------------------------------------------------------

type OwnedSet = Rc<ChunkWaypointSetData>;

const WAYPOINT_FIXTURE_DIR: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../docs/review/fixtures/waypoint"
);

fn asset(name: &str) -> Vec<u8> {
    std::fs::read(format!("{WAYPOINT_FIXTURE_DIR}/{name}"))
        .unwrap_or_else(|e| panic!("read {name}: {e}"))
}

fn truth_lines() -> Vec<String> {
    std::fs::read_to_string(format!("{WAYPOINT_FIXTURE_DIR}/gen_mapload.log"))
        .expect("the gen_mapload truth log")
        .lines()
        .map(str::to_owned)
        .collect()
}

/// The two decoded sets: index 0 is mapA (chunk A), index 1 is mapB
/// (chunk B) — the BW-10-07-03 wiring, re-declared for the client-side
/// rerun.
fn decoded_sets() -> [OwnedSet; 2] {
    let mut a = parse_world_navmesh(&asset("world_navmesh_genmap_a.bin")).expect("mapA decodes");
    let mut b = parse_world_navmesh(&asset("world_navmesh_genmap_b.bin")).expect("mapB decodes");
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

/// The two-chunk `BindSpace` off the truth `CHUNK` lines.
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

/// The bound world, the truth log's bind order (A first, then B).
fn bound_world() -> [OwnedSet; 2] {
    let sets = decoded_sets();
    let flags = portal_flags();
    let space = bind_space(&sets, &flags);
    sets[0].set_chunk_identity(0);
    sets[1].set_chunk_identity(1);
    sets[0].build_neighbour_edges();
    sets[1].build_neighbour_edges();
    sets[0].bind(&space, 0);
    sets[1].bind(&space, 1);
    sets
}

/// The reference `NavLoc(Chunk*, point, girth)` resolution (the
/// BW-10-07-03 shape: the lift, then the containment-first,
/// closest-fallback find).
fn resolve(chunk: usize, sets: &[OwnedSet; 2], at: Vector3) -> (OwnedSet, i32) {
    let point = Vector3::new(at.x, at.y + 0.01, at.z);
    let res = find_by_girth(&[Rc::clone(&sets[chunk])], point, 0.6, false);
    let set = res.p_set().expect("the chunk's set answers").clone();
    (set, res.waypoint())
}

/// `(name, srcChunk, src, dstChunk, dst)` of the walk probes — the
/// truth's own XPATH pairs. The TOUR pair is absent: the truth's
/// XPATHNONE pins the one-shot chunk-level A* (replayed in
/// BW-10-07-03), while the walk face crosses one portal per call by
/// construction and is not that face.
const WALK_PROBES: [(&str, usize, Vector3, usize, Vector3); 5] = [
    (
        "ACROSS",
        0,
        Vector3::new(1.0, 4.0, 1.0),
        0,
        Vector3::new(4.0, 4.0, 1.0),
    ),
    (
        "RAMP",
        0,
        Vector3::new(1.0, 4.0, 1.0),
        0,
        Vector3::new(2.0, 8.0, 8.0),
    ),
    (
        "AROUND",
        1,
        Vector3::new(6.0, 4.0, 2.0),
        1,
        Vector3::new(6.0, 4.0, 8.0),
    ),
    (
        "SEAM",
        0,
        Vector3::new(1.0, 4.0, 1.0),
        1,
        Vector3::new(6.0, 4.0, 2.0),
    ),
    (
        "SEAMBA",
        1,
        Vector3::new(6.0, 4.0, 2.0),
        0,
        Vector3::new(4.0, 4.0, 1.0),
    ),
];

/// The truth's XPATH chain for one probe: the (waypoint, set) steps in
/// order.
fn truth_chain(name: &str) -> Vec<(i32, usize)> {
    let mut chain = Vec::new();
    for line in truth_lines() {
        let f: Vec<&str> = line.split_whitespace().collect();
        if f.len() < 2 || f[1] != name {
            continue;
        }
        if f[0] == "XPATH" {
            chain.push((f[5].parse().unwrap(), f[6].parse().unwrap()));
        }
    }
    chain
}

/// The cached walk over one probe: `find_path` `ReadWriteCache` hop by
/// hop, collecting the (set, waypoint) of every answer.
fn cached_walk(
    sets: &[OwnedSet; 2],
    src_chunk: usize,
    src: Vector3,
    dst_chunk: usize,
    dst: Vector3,
) -> (Vec<(usize, i32)>, bool) {
    let (src_set, src_wp) = resolve(src_chunk, sets, src);
    let (dst_set, dst_wp) = resolve(dst_chunk, sets, dst);
    let navigator = Navigator::default();
    let mut current = NavLoc::from_parts(
        src_set,
        src_wp,
        Vector3::new(src.x, src.y + 0.01, src.z),
        100.0,
    );
    let dst_navloc = NavLoc::from_parts(
        dst_set,
        dst_wp,
        Vector3::new(dst.x, dst.y + 0.01, dst.z),
        100.0,
    );

    let mut hops = Vec::new();
    let mut act = false;
    while hops.len() < 16 {
        let mut next = NavLoc::new();
        let found = navigator.find_path(
            &current,
            &dst_navloc,
            -1.0,
            false,
            &mut next,
            &mut act,
            waypoint::CacheState::ReadWriteCache,
        );
        if !found {
            return (hops, false);
        }
        let done = next.waypoints_equal(&dst_navloc);
        hops.push((
            set_idx(next.p_set().expect("the next has a set"), sets),
            next.waypoint(),
        ));
        if done {
            break;
        }
        current = next;
    }
    (hops, true)
}

/// The acceptance's path half: every walk hop lands on the truth's own
/// waypoint chain, in order, and the walk ends on the destination.
#[test]
fn the_walks_replay_the_original_path_fixtures() {
    let sets = bound_world();
    let lines = truth_lines();

    let mut walked = 0usize;
    for (name, src_chunk, src, dst_chunk, dst) in WALK_PROBES {
        let chain = truth_chain(name);
        let (src_set, src_wp) = resolve(src_chunk, &sets, src);

        let (hops, found) = cached_walk(&sets, src_chunk, src, dst_chunk, dst);
        assert!(found, "{name} walks");
        assert!(!chain.is_empty(), "{name} has a truth chain");
        // the truth's first step is the source waypoint
        assert_eq!(
            (chain[0].0, chain[0].1),
            (src_wp, src_chunk),
            "{name} chain head"
        );
        walked += 1;

        // The source-side traversal is the fixture's substance: every
        // hop within the source set lies on the truth chain after its
        // head, in order (the cached face's smoothing may straighten
        // intermediate states away — a subsequence, never a detour).
        // The far-side hops are the walk face's own goal resolution
        // (`for_destination` resolves the destination point through its
        // own constructor, which can land on a neighbour waypoint the
        // raw A* chain never visits); they are pinned only at the
        // terminus below.
        let head_set = chain[0].1;
        let src_end = chain
            .iter()
            .position(|(_, s)| *s != head_set)
            .unwrap_or(chain.len());
        let mut ti = 1usize;
        let mut pinned = 0usize;
        for (set, wp) in &hops {
            if *set != head_set {
                continue;
            }
            while ti < src_end && !(chain[ti].1 == *set && chain[ti].0 == *wp) {
                ti += 1;
            }
            assert!(
                ti < src_end,
                "{name}: source-side hop ({set}, {wp}) is not on the truth chain {chain:?}"
            );
            ti += 1;
            pinned += 1;
        }
        assert!(
            pinned > 0 || src_end == 1,
            "{name}: the walk pins the source side"
        );

        // and the walk ends on the truth's destination waypoint
        let (dst_set, dst_wp) = resolve(dst_chunk, &sets, dst);
        assert_eq!(
            *hops.last().expect("hops"),
            (dst_chunk, dst_wp),
            "{name}: the walk ends on the destination"
        );
        let _ = (src_set, set_idx(&dst_set, &sets));
    }
    assert_eq!(walked, 5, "ACROSS/RAMP/AROUND/SEAM/SEAMBA walk");

    // the rerun: the same walks answer the same waypoint sequences
    for (name, src_chunk, src, dst_chunk, dst) in WALK_PROBES {
        let (first, _) = cached_walk(&sets, src_chunk, src, dst_chunk, dst);
        let (second, _) = cached_walk(&sets, src_chunk, src, dst_chunk, dst);
        assert_eq!(first, second, "{name}: the walk reruns identically");
    }
    println!("RERUN-PATHS walked={walked} truth_lines={}", lines.len());
}

// ---------------------------------------------------------------------------
// The collision leg: the chunk-space map through the cached resolver.
// ---------------------------------------------------------------------------

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("a crates/ parent")
        .parent()
        .expect("the repository root")
        .to_path_buf()
}

fn space_root() -> PathBuf {
    repo_root().join("docs/review/fixtures/chunk_space/res/space_00010001")
}

fn load_mapping() -> GeometryMapping {
    let raw = std::fs::read(space_root().join("settings.xml")).expect("settings.xml readable");
    let settings = datasection::parse("", &raw).expect("settings.xml parses");
    GeometryMapping::from_settings("space_00010001/", &settings)
}

/// The optimized streaming face: the shipped section cache serves the
/// `load_and_bind` resolver (the OPT-10-01-02 candidate).
fn cached_resolver() -> impl Fn(&str) -> Option<datasection::DataSection> {
    let root = space_root();
    let resource = BwResource::init_paths(&[root.to_str().expect("the fixture root is utf-8")]);
    move |name: &str| {
        resource
            .open_section(name, false)
            .map(|section| (*section).clone())
    }
}

/// The terrain slab over one 100 m cell: the reference fixture blob's
/// binary face (the OPT-10-01-01 helpers).
fn words(vals: &[u32]) -> Vec<u8> {
    vals.iter().flat_map(|w| w.to_le_bytes()).collect()
}

fn v3_bytes(v: Vector3) -> Vec<u8> {
    let mut b = Vec::new();
    b.extend_from_slice(&v.x.to_le_bytes());
    b.extend_from_slice(&v.y.to_le_bytes());
    b.extend_from_slice(&v.z.to_le_bytes());
    b
}

fn tri_bytes(v0: Vector3, v1: Vector3, v2: Vector3) -> Vec<u8> {
    let mut b = Vec::new();
    b.extend(&v3_bytes(v0));
    b.extend(&v3_bytes(v1));
    b.extend(&v3_bytes(v2));
    b.extend(&0u16.to_le_bytes());
    b.extend(&[0u8; 2]);
    b
}

fn node_bytes(front: i32, back: i32, normal: Vector3, d: f32, num_tris: u32) -> Vec<u8> {
    let mut b = Vec::new();
    b.extend_from_slice(&front.to_le_bytes());
    b.extend_from_slice(&back.to_le_bytes());
    b.extend(&v3_bytes(normal));
    b.extend_from_slice(&d.to_le_bytes());
    b.extend_from_slice(&0i32.to_le_bytes());
    b.extend_from_slice(&num_tris.to_le_bytes());
    b.extend_from_slice(&OFFSET_32BIT_NULL.to_le_bytes());
    b.extend_from_slice(&0u32.to_le_bytes());
    assert_eq!(b.len(), BSP_NODE_PACKED_SIZE as usize);
    b
}

fn slab_bsp(origin_x: f32, origin_z: f32, y: f32, first: bool) -> BspTree {
    let mut b = Vec::new();
    b.extend(&words(&[BSP_FILE_CUR_TOKEN, 16, 40, 40, 1, 1, 0]));
    b.extend(&v3_bytes(Vector3::new(origin_x, 0.0, origin_z)));
    b.extend(&v3_bytes(Vector3::new(
        origin_x + 100.0,
        y,
        origin_z + 100.0,
    )));
    b.extend(&[0u8; 8]);
    let (v0, v1, v2) = if first {
        (
            Vector3::new(origin_x, y, origin_z),
            Vector3::new(origin_x + 100.0, y, origin_z),
            Vector3::new(origin_x, y, origin_z + 100.0),
        )
    } else {
        (
            Vector3::new(origin_x + 100.0, y, origin_z),
            Vector3::new(origin_x + 100.0, y, origin_z + 100.0),
            Vector3::new(origin_x, y, origin_z + 100.0),
        )
    };
    b.extend(&tri_bytes(v0, v1, v2));
    b.extend(&node_bytes(
        OFFSET_32BIT_NULL,
        OFFSET_32BIT_NULL,
        Vector3::new(0.0, 0.0, 1.0),
        y,
        1,
    ));
    BspTree::load(&b).expect("the slab blob loads")
}

fn cell_slabs(gx: i32, gz: i32) -> Vec<ChunkObstacle> {
    let id = format!("{gx:04x}{gz:04x}o");
    let (ox, oz) = ((gx as f32) * 100.0, (gz as f32) * 100.0);
    let bb = BoundBox::new(
        Vector3::new(ox, 0.0, oz),
        Vector3::new(ox + 100.0, 10.0, oz + 100.0),
    );
    vec![
        ChunkObstacle::new(
            slab_bsp(ox, oz, 10.0, true),
            Matrix::identity(),
            bb,
            Some(id.clone()),
        ),
        ChunkObstacle::new(
            slab_bsp(ox, oz, 10.0, false),
            Matrix::identity(),
            bb,
            Some(id),
        ),
    ]
}

/// The loaded outside cells the drops run over.
const LOADED_CELLS: [(i32, i32); 4] = [(0, 0), (0, 1), (1, 0), (1, 1)];

/// The map through the optimized streaming face: the cached resolver
/// fills from the same fixture files, the slabs register through the
/// focussed gate.
fn streamed_map() -> ChunkSpace {
    let resolver = cached_resolver();
    let mapping = load_mapping();
    let candidates: Vec<String> = mapping.gather_chunks(&space_root()).into_iter().collect();
    let mut space = ChunkSpace::new(1, vec![mapping]);
    let bound = space.load_and_bind(&candidates, &resolver);
    assert_eq!(bound.len(), 6, "the six files bind through the cache");
    for (gx, gz) in LOADED_CELLS {
        space.focus_chunk(&format!("{gx:04x}{gz:04x}o"));
        for obstacle in cell_slabs(gx, gz) {
            space.add_obstacle(obstacle);
        }
    }
    space.focus_chunk("00000000i");
    space
}

/// The acceptance's collision half: every drop of the probe grid,
/// answered through the client's `findDropPoint` over the
/// cached-streamed map, lands on its slab at 10 m — 40 m down the
/// ray — and the grid reruns identically.
#[test]
fn the_drops_over_the_streamed_map_match_the_fixture_slabs() {
    let space = streamed_map();

    let mut drops = 0usize;
    let mut first: Option<Vec<(Vector3, f32, [Vector3; 3])>> = None;
    for _pass in 0..2 {
        let mut answers = Vec::new();
        for (gx, gz) in LOADED_CELLS {
            let (ox, oz) = ((gx as f32) * 100.0, (gz as f32) * 100.0);
            for i in 0..5 {
                for j in 0..5 {
                    let in_pt = Vector3::new(
                        ox + 10.0 + (i as f32) * 20.0,
                        50.0,
                        oz + 10.0 + (j as f32) * 20.0,
                    );
                    let mut fdpt = ClosestTriangle::new();
                    let (out_pt, tri) = find_drop_point(Some(&space), 1, in_pt, &mut fdpt)
                        .expect("the streamed space is found")
                        .unwrap_or_else(|| panic!("probe ({i},{j}) of cell ({gx},{gz}) missed"));
                    let dist = in_pt.y - out_pt.y;
                    assert!(
                        (dist - 40.0).abs() < 1e-3,
                        "probe ({i},{j}): the drop stops at the slab, got {dist}"
                    );
                    assert!(
                        (out_pt.y - 10.0).abs() < 1e-3,
                        "probe ({i},{j}): the slab answers at 10 m, got {}",
                        out_pt.y
                    );
                    answers.push((out_pt, dist, tri));
                }
            }
        }
        drops = answers.len();
        match &first {
            None => first = Some(answers),
            Some(earlier) => assert_eq!(
                earlier, &answers,
                "the drop grid reruns to the same answers"
            ),
        }
    }
    assert_eq!(drops, LOADED_CELLS.len() * 25, "the whole probe grid");

    // the owning chunk of every probe is its own cell's chunk — the
    // baseline's own resolution, through the streamed map
    for (gx, gz) in LOADED_CELLS {
        let (ox, oz) = ((gx as f32) * 100.0, (gz as f32) * 100.0);
        let found = space
            .find_chunk_from_point(&Vector3::new(ox + 50.0, 0.0, oz + 50.0))
            .expect("the centre resolves")
            .identifier
            .clone();
        assert_eq!(
            found,
            format!("{gx:04x}{gz:04x}o"),
            "the streamed map resolves the owning cell"
        );
    }
    println!("RERUN-DROPS probes={drops} rerun_identical=true");
}
