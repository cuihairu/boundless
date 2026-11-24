//! BW-10-03-04 integration: the reference-generated BSP fixtures
//! (`docs/review/fixtures/physics/bsp/`) must load through the port and
//! answer every probe in the printed ground truth (`gen_bsp.log`, dumped
//! by the unmodified BigWorld `BSPTreeTool::buildBSP` / `loadBSP` /
//! `saveBSPInMemory` faces):
//!
//! - `built.bsp` — the original constructor partitions 11 triangles and
//!   the original writer saves the current-format blob (BT lines);
//! - `legacy.bsp` — a legacy blob per `loadBSPLegacy`'s byte layout,
//!   loaded through the original loader (LG lines);
//! - `legacy_saved.bsp` — the original save of that converted tree; the
//!   port must load both to the same tree, proving the port's legacy
//!   conversion equals the engine's;
//! - `unit_cube.bsp` — the bsp2 child of the real `unit_cube.primitives`,
//!   extracted with the original BinSection parser and loaded through the
//!   original loader (UC lines): a real scene asset, not a hand-made one.

use foundation::math::{PlaneEq, Vector3};
use physics::{BSP_FILE_CUR_TOKEN, BspNode, BspTree, WorldTriangle};
use physics::{OFFSET_32BIT_NULL, PLANE_EQ_SIZE, WORLD_TRIANGLE_SIZE};

const FIXTURE_DIR: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../docs/review/fixtures/physics/bsp"
);

fn fixture_bytes(name: &str) -> Vec<u8> {
    std::fs::read(format!("{FIXTURE_DIR}/{name}")).expect("fixture")
}

fn truth_lines() -> Vec<String> {
    std::fs::read_to_string(format!("{FIXTURE_DIR}/gen_bsp.log"))
        .expect("truth log")
        .lines()
        .map(str::to_owned)
        .collect()
}

fn bits(hex: &str) -> f32 {
    f32::from_bits(u32::from_str_radix(hex, 16).expect("hex bits"))
}

/// A run of hex fields as owned strings (the truth-line shape).
fn row<const N: usize>(f: &[&str]) -> [String; N] {
    f.iter()
        .map(|s| s.to_string())
        .collect::<Vec<_>>()
        .try_into()
        .ok()
        .unwrap()
}

fn v3(hex: [String; 3]) -> Vector3 {
    Vector3::new(bits(&hex[0]), bits(&hex[1]), bits(&hex[2]))
}

fn v3h(hex: [&str; 3]) -> Vector3 {
    v3(row(&hex))
}

/// One RAY truth line: seed/dist bits and, on a hit, the hit point,
/// the triangle normal and the flags.
#[derive(Clone)]
struct RayLine {
    seed_bits: u32,
    dist_bits: u32,
    hit: Option<([String; 3], [String; 3], u16)>,
}

/// One dump_tree run in the truth log: tag, NT/NN/BB/CAN/TRI/USER/RAY lines.
struct Truth {
    nt: u32,
    nn: u32,
    bb: [String; 6],
    can: bool,
    tris: Vec<(u32, [String; 9], u16)>,
    user: Vec<(u32, Vec<u8>)>,
    rays: Vec<RayLine>,
}

fn truth(tag: &'static str) -> Truth {
    let mut t = Truth {
        nt: 0,
        nn: 0,
        bb: Default::default(),
        can: false,
        tris: Vec::new(),
        user: Vec::new(),
        rays: Vec::new(),
    };
    let mut saw_result_ok = false;
    for line in truth_lines() {
        let f: Vec<&str> = line.split_whitespace().collect();
        if f.first().copied() != Some(tag) {
            continue;
        }
        match f[1] {
            // UC SOURCE just records the blob's byte count.
            "SOURCE" => continue,
            "NT" => t.nt = f[2].parse().unwrap(),
            "NN" => t.nn = f[2].parse().unwrap(),
            "BB" => t.bb = row(&f[2..8]),
            "CAN" => t.can = f[2] == "1",
            "TRI" => t.tris.push((
                f[2].parse().unwrap(),
                row(&f[3..12]),
                u16::from_str_radix(f[12], 16).unwrap(),
            )),
            "USER" => {
                let key: u32 = f[2].parse().unwrap();
                let len: usize = f[3].parse().unwrap();
                let data: Vec<u8> = f[4..4 + len]
                    .iter()
                    .map(|h| u8::from_str_radix(h, 16).unwrap())
                    .collect();
                t.user.push((key, data));
            }
            "RAY" => {
                let hit = if f[2] == "HIT" {
                    Some((
                        row(&f[5..8]),
                        row(&f[8..11]),
                        u16::from_str_radix(f[11], 16).unwrap(),
                    ))
                } else {
                    assert_eq!(f[2], "MISS");
                    None
                };
                t.rays.push(RayLine {
                    seed_bits: u32::from_str_radix(f[3], 16).unwrap(),
                    dist_bits: u32::from_str_radix(f[4], 16).unwrap(),
                    hit,
                });
            }
            "RESULT" => {
                assert_eq!(f[2], "OK");
                saw_result_ok = true;
            }
            other => panic!("unexpected {tag} line: {other}"),
        }
    }
    assert!(saw_result_ok, "the reference run reported {tag} RESULT OK");
    t
}

/// The full log replay for one fixture: counts, box bits, every triangle
/// row bit-for-bit, canCollide and the user-data blobs.
fn replay(tag: &'static str, file: &str) -> BspTree {
    let t = truth(tag);
    let tree = BspTree::load(&fixture_bytes(file)).unwrap_or_else(|e| panic!("{tag}: {e}"));

    assert_eq!(tree.num_triangles(), t.nt, "{tag} NT");
    assert_eq!(tree.num_nodes(), t.nn, "{tag} NN");
    assert_eq!(
        tree.bounding_box().min,
        v3(t.bb[0..3].to_vec().try_into().unwrap()),
        "{tag} BB min"
    );
    assert_eq!(
        tree.bounding_box().max,
        v3(t.bb[3..6].to_vec().try_into().unwrap()),
        "{tag} BB max"
    );
    assert_eq!(tree.can_collide(), t.can, "{tag} CAN");

    assert_eq!(tree.triangles().len(), t.tris.len(), "{tag} TRI count");
    for (i, (idx, verts, flags)) in t.tris.iter().enumerate() {
        assert_eq!(*idx, i as u32, "{tag} TRI order");
        let expected = WorldTriangle::new(
            v3(verts[0..3].to_vec().try_into().unwrap()),
            v3(verts[3..6].to_vec().try_into().unwrap()),
            v3(verts[6..9].to_vec().try_into().unwrap()),
            *flags,
        );
        assert_eq!(&tree.triangles()[i], &expected, "{tag} TRI {i} bits");
    }

    for (key, data) in &t.user {
        assert_eq!(
            tree.user_data(*key),
            Some(data.as_slice()),
            "{tag} USER {key}"
        );
    }
    tree
}

#[test]
fn the_original_build_answers_every_truth_probe() {
    let tree = replay("BT", "built.bsp");

    // Node structure of the built tree (partition at z=0: the root keeps
    // the two triangles on its plane, the front child the eight below,
    // the back child the one above — read back from the saved blob).
    let n0 = tree.root();
    assert_eq!(n0.front, Some(1));
    assert_eq!(n0.back, Some(2));
    assert_eq!(
        n0.plane_eq,
        PlaneEq::new(v3h(["00000000", "00000000", "3f800000"]), 0.0)
    );
    assert_eq!(n0.triangles, Some(0));
    assert_eq!(n0.num_triangles, 2);
    assert_eq!(n0.shared, None);

    for (i, run_start, run_len) in [(1usize, 2u32, 8u32), (2, 10, 1)] {
        let n = &tree.nodes()[i];
        assert_eq!(n.front, None, "leaf {i} front");
        assert_eq!(n.back, None, "leaf {i} back");
        assert_eq!(n.triangles, Some(run_start as usize), "leaf {i} run");
        assert_eq!(n.num_triangles, run_len, "leaf {i} run length");
    }

    // The built fixture stores no user data.
    assert_eq!(tree.user_data(0), None);
    assert_eq!(tree.user_data(1), None);
}

#[test]
fn the_legacy_blob_converts_exactly_like_the_engine() {
    // Full-tree equality between the port's legacy conversion of
    // legacy.bsp and the engine's own conversion (saved by
    // saveBSPInMemory as legacy_saved.bsp): node table, runs, shared
    // pointers, box, triangles and user data all compare equal.
    let converted = replay("LG", "legacy.bsp");
    let engine = BspTree::load(&fixture_bytes("legacy_saved.bsp")).expect("engine save loads");
    assert_eq!(converted, engine);

    // The engine-saved structure: n2 shares t0, n4 shares t1 (the two
    // offsets in the blob are element indices 0 and 1).
    assert_eq!(engine.nodes().len(), 5);
    let expect = [
        (Some(1usize), Some(4usize), Some(0usize), 2),
        (Some(2), Some(3), Some(2), 1),
        (None, None, None, 0),
        (None, None, Some(3), 1),
        (None, None, None, 0),
    ];
    for (i, (front, back, tri_run, num)) in expect.into_iter().enumerate() {
        let n = &engine.nodes()[i];
        assert_eq!(n.front, front, "node {i} front");
        assert_eq!(n.back, back, "node {i} back");
        assert_eq!(n.triangles, tri_run, "node {i} run");
        assert_eq!(n.num_triangles, num, "node {i} run length");
    }
    // n2 shared Some(0) num 1, n4 shared Some(1) num 1: the runs derefer-
    // ence to t0 and t1 — the blob stores element indices, not bytes.
    assert_eq!(engine.nodes()[2].shared, Some(0));
    assert_eq!(engine.nodes()[2].num_shared, 1);
    assert_eq!(engine.nodes()[4].shared, Some(1));
    assert_eq!(engine.nodes()[4].num_shared, 1);
    let n2_shared: Vec<_> = engine.node_shared_triangles(&engine.nodes()[2]).collect();
    let n4_shared: Vec<_> = engine.node_shared_triangles(&engine.nodes()[4]).collect();
    assert_eq!(n2_shared, vec![engine.triangles().first().unwrap()]);
    assert_eq!(n4_shared, vec![&engine.triangles()[1]]);
}

#[test]
fn the_real_scene_asset_answers_every_truth_probe() {
    let tree = replay("UC", "unit_cube.bsp");

    // The stale-era asset's structure: a root with only a back child,
    // owning the two triangles on its plane; the child owns the other
    // ten. No shared pointers, no user data.
    let n0 = tree.root();
    assert_eq!(n0.front, None);
    assert_eq!(n0.back, Some(1));
    assert_eq!(
        n0.plane_eq,
        PlaneEq::new(v3h(["00000000", "bf800000", "00000000"]), 0.0)
    );
    assert_eq!(n0.triangles, Some(0));
    assert_eq!(n0.num_triangles, 2);
    let n1 = &tree.nodes()[1];
    assert_eq!(n1.front, None);
    assert_eq!(n1.back, None);
    assert_eq!(
        n1.plane_eq,
        PlaneEq::new(v3h(["3f800000", "00000000", "00000000"]), 0.0)
    );
    assert_eq!(n1.triangles, Some(2));
    assert_eq!(n1.num_triangles, 10);
    assert_eq!(tree.user_data(1), None);
}

#[test]
fn fixture_layouts_match_the_reference_format() {
    // built.bsp and unit_cube.bsp are current-format blobs of the same
    // size: 60 header/box bytes, triangles, shared offsets, nodes.
    for name in ["built.bsp", "unit_cube.bsp"] {
        let blob = fixture_bytes(name);
        let (nt, nn, nsh) = {
            let w = |i: usize| u32::from_le_bytes(blob[i * 4..i * 4 + 4].try_into().unwrap());
            (w(4) as usize, w(5) as usize, w(6) as usize)
        };
        let expect = 60 + nt * WORLD_TRIANGLE_SIZE + nsh * 4 + nn * 40;
        assert_eq!(blob.len(), expect, "{name} size");
        assert_eq!(
            &blob[0..4],
            &BSP_FILE_CUR_TOKEN.to_le_bytes(),
            "{name} token"
        );
        assert_eq!(
            &blob[4..8],
            &PLANE_EQ_SIZE.to_le_bytes(),
            "{name} plane size"
        );
        // The bounding box section carries the two zero Outcode words
        // behind min/max.
        assert_eq!(&blob[52..60], &[0u8; 8], "{name} box outcodes");
    }
    // legacy.bsp keeps the 1.x header: token, triangles, nodes, skipped
    // max-triangles word.
    let legacy = fixture_bytes("legacy.bsp");
    assert_eq!(&legacy[0..4], &0x0050_5342u32.to_le_bytes(), "legacy token");
    assert_eq!(&legacy[4..8], &4u32.to_le_bytes(), "legacy triangles");
    assert_eq!(&legacy[8..12], &5u32.to_le_bytes(), "legacy nodes");
    assert_eq!(&legacy[12..16], &0u32.to_le_bytes(), "legacy max skipped");
    // 5 node records (19 bytes + 2 per reference: 6 references total)
    // and two user-data entries ("abcd" and "z").
    assert_eq!(
        legacy.len(),
        16 + 4 * WORLD_TRIANGLE_SIZE + 5 * 19 + 6 * 2 + (8 + 4) + (8 + 1),
        "legacy size"
    );
}

#[test]
fn offset_decoding_keeps_the_reference_null_semantics() {
    // OFFSET_32BIT_NULL is -1; the node words of legacy_saved.bsp's leaf
    // nodes store it for front/back/triangles, and the port maps it to
    // None while any other value decodes as an element index.
    assert_eq!(OFFSET_32BIT_NULL, -1);
    let tree = BspTree::load(&fixture_bytes("legacy_saved.bsp")).expect("loads");
    for i in [2usize, 4] {
        assert_eq!(tree.nodes()[i].front, None, "leaf {i} front is null");
        assert_eq!(tree.nodes()[i].back, None, "leaf {i} back is null");
    }
    // A malformed negative offset (not the null marker) panics like the
    // reference MF_ASSERT.
    let mut blob = fixture_bytes("built.bsp");
    let node_at = 60 + 11 * WORLD_TRIANGLE_SIZE;
    let save = blob[node_at..node_at + 4].to_vec();
    blob[node_at..node_at + 4].copy_from_slice(&(-3i32).to_le_bytes());
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _ = BspTree::load(&blob);
    }));
    assert!(r.is_err(), "MF_ASSERT(offset >= 0) fires");
    blob[node_at..node_at + 4].copy_from_slice(&save);
}

#[test]
fn the_node_type_is_the_packed_record() {
    // BspNode mirrors the on-disk BSPNode: two child offsets, PlaneEq,
    // a triangle run and a shared run.
    let n = BspNode {
        front: Some(1),
        back: None,
        plane_eq: PlaneEq::new(foundation::math::Vector3::new(0.0, 1.0, 0.0), 2.5),
        triangles: Some(3),
        num_triangles: 4,
        shared: Some(5),
        num_shared: 6,
    };
    assert_eq!(n.front, Some(1));
    assert_eq!(n.back, None);
    assert_eq!(
        *n.plane_eq.normal(),
        foundation::math::Vector3::new(0.0, 1.0, 0.0)
    );
    assert_eq!(n.plane_eq.d(), 2.5);
    assert_eq!(n.triangles, Some(3));
    assert_eq!(n.num_triangles, 4);
    assert_eq!(n.shared, Some(5));
    assert_eq!(n.num_shared, 6);
}

#[test]
fn the_reference_rays_are_answered_bit_for_bit() {
    // The same rays the generator pushed through the original
    // BSPTree::intersects, replayed through the port: the returned
    // triangle (flags, reference-order normal), the narrowed dist and
    // the hit point (start + dist * (end - start)) must match the truth
    // bits exactly; a MISS must leave the caller's dist untouched.
    let bt_rays: [(Vector3, Vector3, f32); 9] = [
        (
            Vector3::new(1.0, 0.5, 3.0),
            Vector3::new(1.0, 0.5, -3.0),
            1.0,
        ),
        (
            Vector3::new(0.5, 1.2, 3.0),
            Vector3::new(0.5, 1.2, -3.0),
            1.0,
        ),
        (
            Vector3::new(0.5, 0.2, -3.0),
            Vector3::new(0.5, 0.2, 1.0),
            1.0,
        ),
        (
            Vector3::new(5.2, 5.2, 6.0),
            Vector3::new(5.2, 5.2, -6.0),
            1.0,
        ),
        (
            Vector3::new(1.0, 0.5, 3.0),
            Vector3::new(1.0, 0.5, -3.0),
            0.45,
        ),
        (
            Vector3::new(1.0, 0.5, 3.0),
            Vector3::new(1.0, 0.5, -3.0),
            0.4,
        ),
        (
            Vector3::new(1.0, 2.0, 3.0),
            Vector3::new(1.0, -1.0, 0.0),
            1.0,
        ),
        (
            Vector3::new(1.0, 2.0, 3.0),
            Vector3::new(1.0, -1.0, 0.0),
            0.4,
        ),
        (
            Vector3::new(1.0, 0.5, -3.0),
            Vector3::new(1.0, 0.5, 3.0),
            1.0,
        ),
    ];
    let lg_rays: [(Vector3, Vector3, f32); 4] = [
        (
            Vector3::new(0.5, 0.5, 3.0),
            Vector3::new(0.5, 0.5, -3.0),
            1.0,
        ),
        (
            Vector3::new(2.5, 0.5, 3.0),
            Vector3::new(2.5, 0.5, -3.0),
            1.0,
        ),
        (
            Vector3::new(0.5, 2.5, -1.0),
            Vector3::new(0.5, 2.5, 4.0),
            1.0,
        ),
        (
            Vector3::new(0.5, 0.5, 0.9),
            Vector3::new(0.5, 0.5, -1.0),
            1.0,
        ),
    ];
    let uc_rays: [(Vector3, Vector3, f32); 3] = [
        (
            Vector3::new(0.0, 0.0, 3.0),
            Vector3::new(0.0, 0.0, -3.0),
            1.0,
        ),
        (
            Vector3::new(3.0, 0.0, 0.0),
            Vector3::new(-3.0, 0.0, 0.0),
            1.0,
        ),
        (
            Vector3::new(5.0, 5.0, 6.0),
            Vector3::new(5.0, 5.0, -6.0),
            1.0,
        ),
    ];
    #[allow(clippy::type_complexity)]
    let trees: [(&str, &str, &[(Vector3, Vector3, f32)]); 3] = [
        ("BT", "built.bsp", &bt_rays),
        ("LG", "legacy.bsp", &lg_rays),
        ("UC", "unit_cube.bsp", &uc_rays),
    ];

    for (tag, file, rays) in trees {
        let t = truth(tag);
        assert_eq!(rays.len(), t.rays.len(), "{tag} ray count");
        let tree = BspTree::load(&fixture_bytes(file)).unwrap_or_else(|e| panic!("{tag}: {e}"));
        for (r, line) in rays.iter().zip(t.rays.iter()) {
            let (s, e, seed) = *r;
            let delta = e - s;
            let mut dist = seed;
            let hit = tree.intersects(s, e, &mut dist, None);
            assert_eq!(seed.to_bits(), line.seed_bits, "{tag} seed");
            assert_eq!(dist.to_bits(), line.dist_bits, "{tag} {s:?} dist");
            match (&line.hit, hit) {
                (Some((point, normal, flags)), Some(tri)) => {
                    assert_eq!(tri.flags(), *flags, "{tag} flags");
                    assert_eq!(
                        tri.normal(),
                        v3(normal.to_vec().try_into().unwrap()),
                        "{tag} normal"
                    );
                    assert_eq!(
                        s + delta * dist,
                        v3(point.to_vec().try_into().unwrap()),
                        "{tag} point"
                    );
                }
                (None, None) => {}
                (expected, got) => panic!(
                    "{tag} hit mismatch: expected {} got {}",
                    expected.is_some(),
                    got.is_some()
                ),
            }
        }
    }
}
