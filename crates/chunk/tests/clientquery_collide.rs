//! BW-10-07-04 acceptance (the fixed-ray half): the client's
//! `BigWorld.findDropPoint` runs `ChunkSpace::collide` under a point —
//! this replays the reference harness's RAY probes
//! (`docs/review/fixtures/waypoint/gen_clientquery.cpp/.log`) over the
//! same world: two focussed shell chunks whose hull faces carry the
//! seam plane, three obstacles whose BSPs are the reference-built
//! fixture blobs, and the chunk the space's own lookup resolves each
//! point through (the CHUNKRES line).
//!
//! The obstacle boxes are the chunk boxes themselves (the harness pins
//! `ChunkObstacle::bb_` at persistent boxes — the reference member is a
//! reference into the caller's storage), and the platform sits low over
//! chunk A with a hole in the slat beneath it.

use chunk::{Chunk, ChunkBoundary, ChunkObstacle, ChunkSpace, GeometryMapping, GridBounds};
use foundation::math::{BoundBox, Matrix, PlaneEq, Vector3};
use physics::{BspTree, ClosestTriangle};

const FIXTURE_DIR: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../docs/review/fixtures/waypoint"
);

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

fn vec_close(p: Vector3, x: f64, y: f64, z: f64) -> bool {
    close(p.x as f64, x) && close(p.y as f64, y) && close(p.z as f64, z)
}

/// The harness's grid-100 mapping over one cell.
fn mapping() -> GeometryMapping {
    GeometryMapping::new(
        "space/",
        100.0,
        GridBounds {
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

/// A loaded shell chunk with the harness's six inward-facing hull
/// planes (front side = the chunk's interior).
fn shell_chunk(
    mapping: &GeometryMapping,
    name: &str,
    min: Vector3,
    max: Vector3,
    faces: [(Vector3, f32); 6],
) -> Chunk {
    let mut chunk = Chunk::new(name, mapping, &Matrix::identity());
    chunk.bounding_box = BoundBox::new(min, max);
    chunk.boundaries = faces
        .iter()
        .map(|(n, d)| ChunkBoundary {
            plane: PlaneEq::new(*n, *d),
            bound_portals: Vec::new(),
            unbound_portals: Vec::new(),
        })
        .collect();
    chunk.bounds = (0..6).collect();
    chunk.is_loaded = true;
    chunk
}

/// The harness world: chunk A west of the seam, chunk B east, the two
/// slats and the platform registered once both chunks are focussed.
fn harness_space() -> ChunkSpace {
    let mapping = mapping();
    let mut space = ChunkSpace::new(7, vec![mapping.clone()]);

    let a_min = Vector3::new(0.0, 0.0, 0.0);
    let a_max = Vector3::new(5.0, 15.0, 10.0);
    let b_min = Vector3::new(5.0, 0.0, 0.0);
    let b_max = Vector3::new(10.0, 20.0, 10.0);

    let chunk_a = shell_chunk(
        &mapping,
        "chunkA",
        a_min,
        a_max,
        [
            // the harness's seam boundary for chunk A carries the
            // portal's own plane (`new ChunkBoundary( planeAB, ... )`),
            // not the inward box face — so A's hull is the x >= 5 slab
            // and A's tag rides the column tree root's front side
            (Vector3::new(1.0, 0.0, 0.0), 5.0),
            (Vector3::new(1.0, 0.0, 0.0), 0.0),
            (Vector3::new(0.0, -1.0, 0.0), -15.0),
            (Vector3::new(0.0, 1.0, 0.0), 0.0),
            (Vector3::new(0.0, 0.0, -1.0), -10.0),
            (Vector3::new(0.0, 0.0, 1.0), 0.0),
        ],
    );
    let chunk_b = shell_chunk(
        &mapping,
        "chunkB",
        b_min,
        b_max,
        [
            (Vector3::new(1.0, 0.0, 0.0), 5.0),
            (Vector3::new(-1.0, 0.0, 0.0), -10.0),
            (Vector3::new(0.0, -1.0, 0.0), -20.0),
            (Vector3::new(0.0, 1.0, 0.0), 0.0),
            (Vector3::new(0.0, 0.0, -1.0), -10.0),
            (Vector3::new(0.0, 0.0, 1.0), 0.0),
        ],
    );
    space.add_chunk(chunk_a);
    space.add_chunk(chunk_b);
    space.focus_chunk("chunkA");
    space.focus_chunk("chunkB");

    let slat_a = ChunkObstacle::new(
        BspTree::load(&asset("clientquery_slatA.bsp")).expect("slat A decodes"),
        Matrix::identity(),
        BoundBox::new(a_min, a_max),
        Some("chunkA".into()),
    );
    let slat_b = ChunkObstacle::new(
        BspTree::load(&asset("clientquery_slatB.bsp")).expect("slat B decodes"),
        Matrix::identity(),
        BoundBox::new(b_min, b_max),
        Some("chunkB".into()),
    );
    let platform = ChunkObstacle::new(
        BspTree::load(&asset("clientquery_platform.bsp")).expect("the platform decodes"),
        Matrix::identity(),
        BoundBox::new(Vector3::new(1.0, 4.0, 1.0), Vector3::new(4.0, 4.0, 4.0)),
        Some("chunkA".into()),
    );
    space.add_obstacle(slat_a);
    space.add_obstacle(slat_b);
    space.add_obstacle(platform);

    space
}

/// The RAY probes the harness hardcodes (`rayProbes[]`): name → the
/// drop point queried.
fn ray_probes() -> [(&'static str, Vector3); 6] {
    [
        ("GA", Vector3::new(1.0, 5.0, 1.0)),
        ("GHOLED", Vector3::new(2.5, 4.0, 1.0)),
        ("GB", Vector3::new(6.5, 7.0, 2.5)),
        ("GPLAT", Vector3::new(2.0, 6.0, 2.0)),
        ("GNONE", Vector3::new(50.0, 5.0, 50.0)),
        ("GSEAM", Vector3::new(5.0, 3.0, 3.0)),
    ]
}

/// The acceptance: every fixed ray of the truth log lands on the same
/// drop point, the same triangle and the same distance — including the
/// miss over the unfocussed corner and the seam point that the column's
/// own tree answers from chunk B's slab.
#[test]
fn the_fixed_rays_match_the_truth_log() {
    let space = harness_space();

    for line in truth_lines() {
        let f = line.split_whitespace().collect::<Vec<_>>();
        match f[0] {
            "RAY" => {
                let name = f[1];
                let in_pt = ray_probes()
                    .iter()
                    .find(|(n, _)| *n == name)
                    .map(|(_, p)| *p)
                    .unwrap_or_else(|| panic!("unknown RAY probe {name}"));

                // the harness's findDropPoint sweep: straight down 100
                let mut fdpt = ClosestTriangle::new();
                let nd_pt = Vector3::new(in_pt.x, in_pt.y - 100.0, in_pt.z);
                let dist = space.collide(in_pt, nd_pt, &mut fdpt);
                assert!(dist >= 0.0, "{name} missed but the truth hit: {line}");

                let out_pt = Vector3::new(in_pt.x, in_pt.y - dist, in_pt.z);
                let tri = fdpt.triangle();

                // RAY <name> in <3> dist <1> out <3> tri <9> flags <1>
                assert!(
                    close(dist as f64, f[7].parse().unwrap()),
                    "{name} dist {line}"
                );
                assert!(
                    vec_close(
                        out_pt,
                        f[9].parse().unwrap(),
                        f[10].parse().unwrap(),
                        f[11].parse().unwrap()
                    ),
                    "{name} out point {line}"
                );
                assert!(
                    vec_close(
                        tri.v0(),
                        f[13].parse().unwrap(),
                        f[14].parse().unwrap(),
                        f[15].parse().unwrap()
                    ) && vec_close(
                        tri.v1(),
                        f[16].parse().unwrap(),
                        f[17].parse().unwrap(),
                        f[18].parse().unwrap()
                    ) && vec_close(
                        tri.v2(),
                        f[19].parse().unwrap(),
                        f[20].parse().unwrap(),
                        f[21].parse().unwrap()
                    ),
                    "{name} triangle {line}"
                );
                assert_eq!(tri.flags(), f[23].parse::<u16>().unwrap(), "{name} flags");
            }
            "RAYMISS" => {
                let name = f[1];
                let in_pt = ray_probes()
                    .iter()
                    .find(|(n, _)| *n == name)
                    .map(|(_, p)| *p)
                    .unwrap_or_else(|| panic!("unknown RAYMISS probe {name}"));

                let mut fdpt = ClosestTriangle::new();
                let nd_pt = Vector3::new(in_pt.x, in_pt.y - 100.0, in_pt.z);
                let dist = space.collide(in_pt, nd_pt, &mut fdpt);
                assert!(dist < 0.0, "{name} hit at {dist} but the truth missed");
            }
            _ => {}
        }
    }

    // all six probes appeared in the log (the loop above would otherwise
    // assert nothing)
    let rays = truth_lines()
        .iter()
        .filter(|l| l.starts_with("RAY ") || l.starts_with("RAYMISS "))
        .count();
    assert_eq!(rays, ray_probes().len(), "every probe has a truth line");
}

/// The space's own chunk lookup resolves both probe points to chunkA
/// (the CHUNKRES line): the seam plane is coplanar in the column's hull
/// tree, so the strictly-larger-hull volume gate flips the contested
/// B-side point to chunk A — the reference's own answer for this
/// fixture.
#[test]
fn the_chunk_lookup_resolves_both_probe_points_to_chunk_a() {
    let space = harness_space();
    assert_eq!(
        space.find_chunk_from_point_exact(&Vector3::new(6.0, 4.01, 2.0)),
        Some("chunkA".into()),
        "the B-side point"
    );
    assert_eq!(
        space.find_chunk_from_point_exact(&Vector3::new(1.0, 4.01, 2.0)),
        Some("chunkA".into()),
        "the A-side point"
    );
}
