//! BW-10-07-04 acceptance (the navigation-display half): the client's
//! `BigWorld.navigatePathPoints` walks the waypoint graph between the
//! drop points through `Navigator::findFullPath` — this replays the
//! reference harness's NAV probes (`gen_clientquery.log`) over the
//! same generated map pair (`world_navmesh_genmap_a/b.bin`, the bind
//! walk of `gen_mapload`), resolving each probe's NavLoc the way the
//! reference `NavLoc(Chunk*, point, girth)` constructor does through
//! the chunk the space's own lookup answers (the CHUNKRES resolution
//! — every probe point answers chunkA).
//!
//! The path points and counts come verbatim from the truth log, with
//! the `almostEqual` prepend of the raw source (the script face).

use client::navigation::{NavigatePathPointsFailure, find_drop_point, navigate_path_points};
use foundation::math::{BoundBox, Matrix, PlaneEq, Vector3};
use physics::ClosestTriangle;
use std::rc::Rc;
use waypoint::{
    BindChunk, BindPortal, BindSpace, ChunkWaypointSetData, NavLoc, Navigator, Portal,
    find_by_girth, parse_world_navmesh,
};

const FIXTURE_DIR: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../docs/review/fixtures/waypoint"
);

const GIRTH: f32 = 0.6;

type OwnedSet = Rc<ChunkWaypointSetData>;

fn asset(name: &str) -> Vec<u8> {
    std::fs::read(format!("{FIXTURE_DIR}/{name}")).unwrap_or_else(|e| panic!("read {name}: {e}"))
}

fn truth_lines() -> Vec<String> {
    std::fs::read_to_string(format!("{FIXTURE_DIR}/gen_clientquery.log"))
        .expect("the gen_clientquery truth log")
        .lines()
        .map(str::to_owned)
        .collect()
}

/// The truth prints %.6f.
fn close(a: f64, truth: f64) -> bool {
    (a - truth).abs() <= 1e-5 * truth.abs().max(1.0)
}

fn vec_close(p: Vector3, x: f32, y: f32, z: f32) -> bool {
    close(p.x as f64, x as f64) && close(p.y as f64, y as f64) && close(p.z as f64, z as f64)
}

fn v(x: f32, y: f32, z: f32) -> Vector3 {
    Vector3::new(x, y, z)
}

/// The harness's grid-100 mapping over one cell.
fn mapping() -> chunk::GeometryMapping {
    chunk::GeometryMapping::new(
        "space/",
        100.0,
        chunk::GridBounds {
            min_l_grid_x: 0,
            min_l_grid_y: 0,
            max_l_grid_x: 1,
            max_l_grid_y: 1,
            min_grid_x: 0,
            min_grid_y: 0,
            max_grid_x: 1,
            max_grid_y: 1,
        },
        false,
    )
}

/// A loaded shell chunk with the harness's six inward hull planes; the
/// seam face carries the portal's own plane (the CHUNKRES answer).
fn shell_chunk(
    mapping: &chunk::GeometryMapping,
    name: &str,
    min: Vector3,
    max: Vector3,
    faces: [(Vector3, f32); 6],
) -> chunk::Chunk {
    let mut chunk = chunk::Chunk::new(name, mapping, &Matrix::identity());
    chunk.bounding_box = BoundBox::new(min, max);
    chunk.boundaries = faces
        .iter()
        .map(|(n, d)| chunk::ChunkBoundary {
            plane: PlaneEq::new(*n, *d),
            bound_portals: Vec::new(),
            unbound_portals: Vec::new(),
        })
        .collect();
    chunk.bounds = (0..6).collect();
    chunk.is_loaded = true;
    chunk
}

/// The camera space: the two shell chunks focussed (the collision
/// scene's columns), the navmesh sets mounted on their chunk
/// identities, neighbour edges built and the bind walk run — the
/// `gen_mapload` wiring.
fn camera_space() -> (chunk::ChunkSpace, Navigator, [OwnedSet; 2]) {
    let sets_raw = [
        parse_world_navmesh(&asset("world_navmesh_genmap_a.bin")).expect("mapA decodes"),
        parse_world_navmesh(&asset("world_navmesh_genmap_b.bin")).expect("mapB decodes"),
    ];
    let mut sets_raw = sets_raw;
    let sets = [
        Rc::new(sets_raw[0].remove(0)),
        Rc::new(sets_raw[1].remove(0)),
    ];

    let mapping = mapping();
    let mut space = chunk::ChunkSpace::new(7, vec![mapping.clone()]);

    let a_min = v(0.0, 0.0, 0.0);
    let a_max = v(5.0, 15.0, 10.0);
    let b_min = v(5.0, 0.0, 0.0);
    let b_max = v(10.0, 20.0, 10.0);

    space.add_chunk(shell_chunk(
        &mapping,
        "chunkA",
        a_min,
        a_max,
        [
            (v(1.0, 0.0, 0.0), 5.0),
            (v(1.0, 0.0, 0.0), 0.0),
            (v(0.0, -1.0, 0.0), -15.0),
            (v(0.0, 1.0, 0.0), 0.0),
            (v(0.0, 0.0, -1.0), -10.0),
            (v(0.0, 0.0, 1.0), 0.0),
        ],
    ));
    space.add_chunk(shell_chunk(
        &mapping,
        "chunkB",
        b_min,
        b_max,
        [
            (v(1.0, 0.0, 0.0), 5.0),
            (v(-1.0, 0.0, 0.0), -10.0),
            (v(0.0, -1.0, 0.0), -20.0),
            (v(0.0, 1.0, 0.0), 0.0),
            (v(0.0, 0.0, -1.0), -10.0),
            (v(0.0, 0.0, 1.0), 0.0),
        ],
    ));
    space.focus_chunk("chunkA");
    space.focus_chunk("chunkB");

    // the harness's three obstacles (the RAY half's scene — the two
    // slats and the platform over chunk A), so the drop-point face
    // answers the same world the navigation walk runs over
    let slat_a = chunk::ChunkObstacle::new(
        physics::BspTree::load(&asset("clientquery_slatA.bsp")).expect("slat A decodes"),
        Matrix::identity(),
        BoundBox::new(a_min, a_max),
        Some("chunkA".into()),
    );
    let slat_b = chunk::ChunkObstacle::new(
        physics::BspTree::load(&asset("clientquery_slatB.bsp")).expect("slat B decodes"),
        Matrix::identity(),
        BoundBox::new(b_min, b_max),
        Some("chunkB".into()),
    );
    let platform = chunk::ChunkObstacle::new(
        physics::BspTree::load(&asset("clientquery_platform.bsp")).expect("the platform decodes"),
        Matrix::identity(),
        BoundBox::new(v(1.0, 4.0, 1.0), v(4.0, 4.0, 4.0)),
        Some("chunkA".into()),
    );
    space.add_obstacle(slat_a);
    space.add_obstacle(slat_b);
    space.add_obstacle(platform);

    // the bind walk (gen_mapload's ChunkWaypointSet::bind): the
    // permissive portal pair on the seam
    let flags = [
        Rc::new(Portal {
            permissive: true,
            has_chunk_item: false,
        }),
        Rc::new(Portal {
            permissive: true,
            has_chunk_item: false,
        }),
    ];
    let seam = |far: usize, flags: &Rc<Portal>| BindPortal {
        plane: PlaneEq::new(v(1.0, 0.0, 0.0), 5.0),
        points: vec![(4.4, 9.9), (4.4, 0.1), (5.6, 0.1), (5.6, 9.9)],
        u_axis: v(1.0, 0.0, 0.0),
        v_axis: v(0.0, 0.0, 1.0),
        origin: v(0.0, 4.1, 0.0),
        far: Some(far),
        flags: Rc::clone(flags),
        unbound: false,
        has_chunk_or_is_nothing: false,
        is_invasive: false,
    };
    let bind_space = BindSpace {
        chunks: vec![
            BindChunk {
                bb: BoundBox::new(a_min, a_max),
                outside: false,
                loaded: true,
                overlappers_loaded: false,
                portals: vec![seam(1, &flags[0])],
                sets: vec![Rc::clone(&sets[0])],
            },
            BindChunk {
                bb: BoundBox::new(b_min, b_max),
                outside: false,
                loaded: true,
                overlappers_loaded: false,
                portals: vec![seam(0, &flags[1])],
                sets: vec![Rc::clone(&sets[1])],
            },
        ],
    };
    sets[0].set_chunk_identity(0);
    sets[1].set_chunk_identity(1);
    // the owning chunk's box rides the mounted-chunk seam: `clip`'s
    // out-of-chunk fallback validates the clipped point against
    // `pChunk->boundingBox()`, which is what pulls a B-side probe that
    // CHUNKRES resolved through chunkA back onto A's seam
    sets[0].set_chunk_bounding_box(BoundBox::new(a_min, a_max));
    sets[1].set_chunk_bounding_box(BoundBox::new(b_min, b_max));
    sets[0].build_neighbour_edges();
    sets[1].build_neighbour_edges();
    sets[0].bind(&bind_space, 0);
    sets[1].bind(&bind_space, 1);

    let navigator = Navigator::default();
    (space, navigator, sets)
}

/// The reference `NavLoc(Chunk*, point, girth)` resolution through the
/// chunk the space's own lookup answers — the CHUNKRES resolution is
/// chunkA for both probe points (the seam plane is coplanar in the
/// column's hull tree and the volume gate keeps the first-fitting
/// hull), so both probe sides resolve through the mapA set.
fn resolve(sets: &[OwnedSet; 2], at: Vector3) -> NavLoc {
    let point = v(at.x, at.y + 0.01, at.z);
    let res = find_by_girth(&[Rc::clone(&sets[0])], point, GIRTH, false);
    let set = res.p_set().expect("chunkA's set answers").clone();
    NavLoc::from_parts(set, res.waypoint(), point, 0.0)
}

/// The NAV probes the harness hardcodes (`navProbes[]`).
fn nav_probes() -> [(&'static str, Vector3, Vector3); 7] {
    [
        ("ACROSS", v(1.0, 4.0, 1.0), v(4.0, 4.0, 1.0)),
        ("RAMP", v(1.0, 4.0, 1.0), v(2.0, 8.0, 8.0)),
        ("AROUND", v(6.0, 4.0, 2.0), v(6.0, 4.0, 8.0)),
        ("SEAM", v(1.0, 4.0, 1.0), v(6.0, 4.0, 2.0)),
        ("SEAMBA", v(6.0, 4.0, 2.0), v(4.0, 4.0, 1.0)),
        ("TOUR", v(1.0, 4.0, 1.0), v(6.0, 4.0, 8.0)),
        ("SAME", v(6.0, 4.0, 2.0), v(6.0, 4.0, 2.0)),
    ]
}

/// The acceptance: every navigation probe walks the truth log's path
/// exactly — the same point count and the same points (the script
/// face's prepend already accounted for in the log).
#[test]
fn the_navigation_paths_match_the_truth_log() {
    let (space, navigator, sets) = camera_space();

    for line in truth_lines() {
        let f = line.split_whitespace().collect::<Vec<_>>();
        if f[0] != "NAVPT" {
            continue;
        }
        let name = f[1];
        let index: usize = f[2].parse().unwrap();
        let truth = v(
            f[3].parse().unwrap(),
            f[4].parse().unwrap(),
            f[5].parse().unwrap(),
        );

        let (_, src, dst) = *nav_probes()
            .iter()
            .find(|(n, _, _)| *n == name)
            .unwrap_or_else(|| panic!("unknown NAVPT probe {name}"));

        let src_navloc = resolve(&sets, src);
        let dst_navloc = resolve(&sets, dst);

        let path = navigate_path_points(
            Some(&space),
            &navigator,
            &src_navloc,
            &dst_navloc,
            src,
            dst,
            500.0,
        )
        .unwrap_or_else(|e| panic!("{name} failed: {:?}", e));

        assert!(
            index < path.len(),
            "{name} point {index} beyond the walked {path:?}"
        );
        assert!(
            vec_close(path[index], truth.x, truth.y, truth.z),
            "{name} point {index} {path:?} vs {line}"
        );
    }

    // the point counts: the NAV lines carry them
    for line in truth_lines() {
        let f = line.split_whitespace().collect::<Vec<_>>();
        if f[0] != "NAV" {
            continue;
        }
        let name = f[1];
        let count: usize = f[3].parse().unwrap();

        let (_, src, dst) = *nav_probes()
            .iter()
            .find(|(n, _, _)| *n == name)
            .unwrap_or_else(|| panic!("unknown NAV probe {name}"));

        let src_navloc = resolve(&sets, src);
        let dst_navloc = resolve(&sets, dst);

        let path = navigate_path_points(
            Some(&space),
            &navigator,
            &src_navloc,
            &dst_navloc,
            src,
            dst,
            500.0,
        )
        .unwrap_or_else(|e| panic!("{name} failed: {:?}", e));

        assert_eq!(path.len(), count, "{name} count vs {line}");
    }
}

/// The missing-camera-space gate raises the reference error text.
#[test]
fn navigate_without_a_camera_space_refuses_with_the_reference_text() {
    let (_, navigator, sets) = camera_space();
    let (_, src, dst) = nav_probes()[0];

    let err = navigate_path_points(
        None,
        &navigator,
        &resolve(&sets, src),
        &resolve(&sets, dst),
        src,
        dst,
        500.0,
    )
    .expect_err("no camera space");
    assert_eq!(err, NavigatePathPointsFailure::NoCameraSpace);
    assert_eq!(
        err.message(),
        "BigWorld.navigatePathPoints: Cannot get camera space"
    );
}

/// The findDropPoint face over the camera space answers the same
/// drop point the truth log's RAY line carries for the probe the NAV
/// walk starts from — the two script faces ride the same space.
#[test]
fn the_drop_point_face_answers_the_truth_over_the_camera_space() {
    let (space, _, _) = camera_space();

    let mut fdpt = ClosestTriangle::new();
    let (out_pt, _tri) = find_drop_point(Some(&space), 7, v(1.0, 5.0, 1.0), &mut fdpt)
        .expect("the camera space answers")
        .expect("GA hits");

    // RAY GA: out 1.000000 4.000000 1.000000
    assert!(vec_close(out_pt, 1.0, 4.0, 1.0), "GA out point {out_pt:?}");
}

/// The lookup miss (no space with the ID) carries the reference error
/// text with the space ID formatted in.
#[test]
fn the_drop_point_face_reports_the_missing_space_with_the_reference_text() {
    let err = find_drop_point(None, 7, v(1.0, 5.0, 1.0), &mut ClosestTriangle::new())
        .expect_err("the lookup missed");
    assert_eq!(err.space_id, 7);
    assert_eq!(err.message(), "BigWorld.findDropPoint(): No space ID 7");
}
