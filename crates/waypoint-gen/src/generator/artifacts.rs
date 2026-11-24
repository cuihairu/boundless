//! Acceptance artifacts for BW-10-05-04: the two scenes of the
//! reference harness `docs/review/fixtures/waypoint/gen_generate.cpp`
//! (slope + clipped shell) re-run through the Rust flood + Rust BSP
//! generator, dumped in the harness's exact `FILL/SCENE/.../HX/R`
//! format and compared line by line against the committed truth log
//! `gen_generate.log` produced by compiling the original
//! `waypoint_generator.cpp`. The remaining tests drive the branches
//! the two scenes never reach (doubled-adjacency collapse, dangling
//! adjacency remap, unreciprocated set links, biggest-polygon set
//! formation, the >25-cell even split, and `joinPolygon`'s error
//! arms) with hand-built polygons.

use super::{SplitDef, VERTICAL_SPLIT};
use crate::flood::MAX_HEIGHTS;
use crate::flood::artifacts::{clipped_chunk, shell_scene, slope_scene};
use crate::flood::tests::{Bounds, FieldPhysics, Slab, grid, seed};
use crate::generator::{BspNode, PolygonDef, VertexDef, WaypointGenerator};
use crate::view::{NavView, as_binary};
use foundation::math::vector::Vector3;

const TRUTH: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../docs/review/fixtures/waypoint/gen_generate.log"
));

fn fnv1a64(data: &[u8]) -> u64 {
    let mut hash: u64 = 14695981039346656037;
    for b in data {
        hash ^= u64::from(*b);
        hash = hash.wrapping_mul(1099511628211);
    }
    hash
}

/// `dumpPolys` (`gen_generate.cpp:190-208`): polygon stream through the
/// NavView face, `VertexRef` scaling applied.
fn dump_polys(what: &str, gener: &WaypointGenerator, out: &mut Vec<String>) {
    out.push(format!(
        "{} polys {} sets {}",
        what,
        gener.polygon_count(),
        gener.sets()
    ));
    for p in 0..gener.polygon_count() {
        out.push(format!(
            "  P {p} set {} minh {:.1} maxh {:.1} vc {}",
            gener.poly_set(p),
            gener.min_height(p),
            gener.max_height(p),
            gener.vertex_count(p) as u32
        ));
        for v in 0..gener.vertex_count(p) {
            let vr = gener.vertex(p, v);
            out.push(format!(
                "  V {v} 0 x {:.2} z {:.2} adj {} chunk {}",
                vr.x,
                vr.z,
                vr.adj_nav_poly,
                i32::from(vr.adj_to_another_chunk)
            ));
        }
    }
}

/// `dumpBinary` (`gen_generate.cpp:143-186`): length + fnv, 16-byte hex
/// lines, then the record-stream decode.
fn dump_binary(what: &str, bin: &[u8], out: &mut Vec<String>) {
    out.push(format!(
        "{} bytes {} fnv {:016x}",
        what,
        bin.len(),
        fnv1a64(bin)
    ));
    let mut i = 0usize;
    while i < bin.len() {
        let mut hex = String::new();
        for j in 0..16 {
            if i + j < bin.len() {
                hex.push_str(&format!("{:02x}", bin[i + j]));
            }
        }
        out.push(format!("HX {:04x} {}", i, hex));
        i += 16;
    }

    let mut ptr = 0usize;
    let mut record = 0;
    while ptr < bin.len() {
        let version = i32::from_le_bytes(bin[ptr..ptr + 4].try_into().unwrap());
        let girth = f32::from_le_bytes(bin[ptr + 4..ptr + 8].try_into().unwrap());
        let nav_poly_elts = i32::from_le_bytes(bin[ptr + 8..ptr + 12].try_into().unwrap());
        let nav_poly_edges = i32::from_le_bytes(bin[ptr + 12..ptr + 16].try_into().unwrap());
        ptr += 16;
        out.push(format!(
            "R {record} version {version} girth {girth:.1} nwp {nav_poly_elts} ne {nav_poly_edges}"
        ));
        let ctrl = ptr;
        ptr += nav_poly_elts as usize * 12;
        for p in 0..nav_poly_elts {
            let c = ctrl + p as usize * 12;
            let minh = f32::from_le_bytes(bin[c..c + 4].try_into().unwrap());
            let maxh = f32::from_le_bytes(bin[c + 4..c + 8].try_into().unwrap());
            let vc = i32::from_le_bytes(bin[c + 8..c + 12].try_into().unwrap());
            out.push(format!("  P {p} minh {minh:.1} maxh {maxh:.1} vc {vc}"));
        }
        for v in 0..nav_poly_edges {
            let x = f32::from_le_bytes(bin[ptr..ptr + 4].try_into().unwrap());
            let z = f32::from_le_bytes(bin[ptr + 4..ptr + 8].try_into().unwrap());
            let adj = i32::from_le_bytes(bin[ptr + 8..ptr + 12].try_into().unwrap());
            ptr += 12;
            out.push(format!("  V {v} x {x:.1} z {z:.1} adj {adj}"));
        }
        record += 1;
    }
}

/// `runScene` (`gen_generate.cpp:210-271`).
fn run_scene(
    name: &str,
    physics: &mut FieldPhysics,
    clip_chunk: bool,
    seeds: &[Vector3],
    out: &mut Vec<String>,
) {
    // --- flood stage
    let mut flood = grid(0.0, 8.0, 1.0);
    let chunk: Bounds = if clip_chunk {
        clipped_chunk()
    } else {
        Bounds::everywhere()
    };

    let mut total = 0;
    for (i, s) in seeds.iter().enumerate() {
        let c = flood.fill(s, physics, &chunk, None);
        out.push(format!("FILL {i} count {c}"));
        total += c;
    }
    flood.postfilter_add();
    flood.postfilter_remove();
    out.push(format!("SCENE {name} count {total}"));

    // --- generator stage: grid copy, generate, streamline, sets
    let mut gener = WaypointGenerator::new();
    gener.init(
        flood.xsize(),
        flood.zsize(),
        &Vector3::new(0.0, 0.0, 0.0),
        1.0,
    );
    for g in 0..MAX_HEIGHTS {
        gener.hgt_grids_mut()[g] = flood.hgt_grids()[g].to_vec();
        gener.adj_grids_mut()[g] = flood.adj_grids()[g].to_vec();
    }

    gener.generate();
    dump_polys("GENERATED", &gener, out);
    dump_binary("GENASBIN", &as_binary(&gener, 0.8), out);

    gener.streamline();
    dump_polys("STREAMLINED", &gener, out);

    gener.calculate_set_membership(seeds, name);
    dump_polys("SETMEM", &gener, out);
    dump_binary(name, &as_binary(&gener, 0.8), out);
}

#[test]
fn the_reference_generator_harness_scenes_reproduce_the_truth_log() {
    let mut out = Vec::new();

    let mut slope = slope_scene();
    run_scene(
        "slope",
        &mut slope,
        false,
        &[seed(0.5, 10.0, 0.5)],
        &mut out,
    );

    let mut shell = shell_scene();
    run_scene(
        "shell",
        &mut shell,
        true,
        &[
            seed(0.5, 10.0, 0.5),
            seed(3.0, 10.0, 3.5),
            seed(2.5, 1.0, 4.5),
        ],
        &mut out,
    );

    let truth: Vec<&str> = TRUTH.lines().collect();
    assert_eq!(
        out.len(),
        truth.len(),
        "line count mismatch; our tail: {:?}",
        out.iter().rev().take(8).collect::<Vec<_>>()
    );
    for (i, (ours, want)) in out.iter().zip(truth.iter()).enumerate() {
        assert_eq!(ours, want, "line {}", i + 1);
    }
}

// --- branch coverage the two harness scenes cannot reach ---------------

fn rect(x0: f32, z0: f32, x1: f32, z1: f32, adjs: [i32; 4]) -> PolygonDef {
    PolygonDef {
        vertices: vec![
            VertexDef {
                pos: (x0, z0),
                adj_nav_poly: adjs[0],
                adj_to_another_chunk: false,
                angles: 0,
            },
            VertexDef {
                pos: (x1, z0),
                adj_nav_poly: adjs[1],
                adj_to_another_chunk: false,
                angles: 0,
            },
            VertexDef {
                pos: (x1, z1),
                adj_nav_poly: adjs[2],
                adj_to_another_chunk: false,
                angles: 2,
            },
            VertexDef {
                pos: (x0, z1),
                adj_nav_poly: adjs[3],
                adj_to_another_chunk: false,
                angles: 2,
            },
        ],
        min_height: 0.0,
        max_height: 0.0,
        set: 0,
    }
}

/// `streamline`'s doubled-adjacency removal, empty-polygon removal and
/// the dangling-id remap fallback (`waypoint_generator.cpp:2733-2800`).
#[test]
fn streamline_collapses_doubled_adjacencies_and_dangling_ids() {
    let mut gener = WaypointGenerator::new();
    // vertex 1 shares polygon 1's adjacency with vertex 0 (doubled);
    // vertex 2 points at polygon 3, which is the empty one below.
    gener.polygons = vec![
        rect(0.0, 0.0, 2.0, 2.0, [1, 1, 3, 0]),
        rect(2.0, 0.0, 4.0, 2.0, [0, 0, 0, 1]),
        PolygonDef::default(),
    ];
    gener.streamline();

    // the empty polygon is gone and the doubled adjacency collapsed
    assert_eq!(gener.polygons.len(), 2);
    assert_eq!(gener.polygons[0].vertices.len(), 3);
    // ids contracted: old 1 -> 1, old 2 -> 2, old 3 (empty) -> -1 -> 0
    assert_eq!(gener.polygons[0].vertices[0].adj_nav_poly, 1);
    assert_eq!(gener.polygons[0].vertices[1].adj_nav_poly, 0);
    assert_eq!(gener.polygons[1].vertices[3].adj_nav_poly, 1);
}

/// `calculateSetMembership`'s unreciprocated-connection zeroing, the
/// same-set continue and the cross-chunk frontier flag
/// (`waypoint_generator.cpp:2950-2965`).
#[test]
fn set_membership_zeroes_unreciprocated_cross_set_links() {
    let mut gener = WaypointGenerator::new();
    gener.init(8, 8, &Vector3::new(0.0, 0.0, 0.0), 1.0);
    gener.polygons = vec![
        // scanned first: its own frontier seed floods it as set 1,
        // together with polygon 3 (diamond: 1 -> 3 -> 1)
        rect(1.0, 0.0, 3.0, 2.0, [3, 0, 0, 0]),
        // scanned second: set 2, and its link back into set 1 is
        // unreciprocated -> zeroed
        rect(-2.0, 0.0, 0.0, 2.0, [1, 0, 0, 0]),
        // flooded from polygon 1; links back into the same set
        rect(4.0, 0.0, 6.0, 2.0, [1, 0, 0, 0]),
    ];
    gener.polygons[1].vertices[1].adj_to_another_chunk = true;

    gener.calculate_set_membership(&[seed(2.0, 1.0, 1.0), seed(-1.0, 1.0, 1.0)], "sets");

    assert_eq!(gener.sets(), 2);
    assert_eq!(gener.polygons[0].set, 1);
    assert_eq!(gener.polygons[2].set, 1);
    assert_eq!(gener.polygons[1].set, 2);
    // the unreciprocated link into set 1 was zeroed
    assert_eq!(gener.polygons[1].vertices[0].adj_nav_poly, 0);
}

/// Without any frontier polygon the single set forms from the biggest
/// polygon, the scan stops there, and every unconnected polygon is
/// dropped by the removal pass (`waypoint_generator.cpp:2917-2990`).
#[test]
fn set_membership_without_frontier_forms_from_the_biggest_polygon() {
    let mut gener = WaypointGenerator::new();
    gener.init(8, 8, &Vector3::new(0.0, 0.0, 0.0), 1.0);
    gener.polygons = vec![
        rect(0.0, 0.0, 2.0, 2.0, [0, 0, 0, 0]),
        rect(0.0, 4.0, 1.0, 5.0, [0, 0, 0, 0]),
    ];

    gener.calculate_set_membership(&[], "nofrontier");

    assert_eq!(gener.sets(), 1);
    // the smaller, unconnected polygon did not join the set and is gone
    assert_eq!(gener.polygons.len(), 1);
    assert_eq!(gener.polygons[0].set, 1);
}

/// `joinPolygon`'s skip arms: empty polygon, zero-sized neighbour and
/// unfindable shared vertices (`waypoint_generator.cpp:2146-2176`).
/// Both live-adjacency cases use a clean two-vertex angle run (`[2, 2,
/// 0, 0]`): `singleAdjacency` only survives when every vertex of the
/// run links to the same polygon.
#[test]
fn join_polygon_skips_empty_and_unfindable_neighbours() {
    let mut gener = WaypointGenerator::new();

    // empty polygons can exist and never join
    gener.polygons = vec![PolygonDef::default()];
    assert!(!gener.join_polygon(0));

    // a live adjacency at a polygon with no vertices
    gener.polygons = vec![
        rect(0.0, 0.0, 2.0, 2.0, [2, 2, 0, 0]),
        PolygonDef::default(),
    ];
    assert!(!gener.join_polygon(0));

    // a live adjacency at a polygon that does not share the vertices
    gener.polygons = vec![
        rect(0.0, 0.0, 2.0, 2.0, [2, 2, 0, 0]),
        rect(10.0, 10.0, 12.0, 12.0, [0, 0, 0, 0]),
    ];
    assert!(!gener.join_polygon(0));
}

/// `joinPolygon`'s merge: a clean shared edge (a two-vertex run with
/// one adjacency on each side) joins into a single polygon, the loser's
/// id disappears from every remaining adjacency and from the bsp node
/// index, and combined heights are widened
/// (`waypoint_generator.cpp:2180-2265`).
#[test]
fn join_polygon_merges_neighbours_and_fixes_up_ids() {
    let mut gener = WaypointGenerator::new();
    gener.init(8, 8, &Vector3::new(0.0, 0.0, 0.0), 1.0);

    // p1: a rect whose east edge carries an intermediate vertex, so the
    // shared segment is the three-point run v1 -> v2 -> v3. vertex
    // angle = the direction of the edge it closes (south 6, east 0,
    // north 2, west 4); v.adj = the polygon across the edge that
    // starts at v.
    let east_run = |adj: i32| -> PolygonDef {
        PolygonDef {
            vertices: vec![
                VertexDef {
                    pos: (0.0, 0.0),
                    adj_nav_poly: 0,
                    adj_to_another_chunk: false,
                    angles: 6,
                },
                VertexDef {
                    pos: (2.0, 0.0),
                    adj_nav_poly: adj,
                    adj_to_another_chunk: false,
                    angles: 0,
                },
                VertexDef {
                    pos: (2.0, 2.0),
                    adj_nav_poly: adj,
                    adj_to_another_chunk: false,
                    angles: 0,
                },
                VertexDef {
                    pos: (2.0, 4.0),
                    adj_nav_poly: 0,
                    adj_to_another_chunk: false,
                    angles: 2,
                },
                VertexDef {
                    pos: (0.0, 4.0),
                    adj_nav_poly: 0,
                    adj_to_another_chunk: false,
                    angles: 4,
                },
            ],
            min_height: 0.0,
            max_height: 1.0,
            set: 0,
        }
    };
    // p2: the east neighbour; its west edge is w3 -> w0, exactly the
    // shared segment reversed. w1 points at the victim (polygon 3).
    let west_side = PolygonDef {
        vertices: vec![
            VertexDef {
                pos: (2.0, 0.0),
                adj_nav_poly: 0,
                adj_to_another_chunk: false,
                angles: 6,
            },
            VertexDef {
                pos: (4.0, 0.0),
                adj_nav_poly: 3,
                adj_to_another_chunk: false,
                angles: 0,
            },
            VertexDef {
                pos: (4.0, 4.0),
                adj_nav_poly: 0,
                adj_to_another_chunk: false,
                angles: 2,
            },
            VertexDef {
                pos: (2.0, 4.0),
                adj_nav_poly: 0,
                adj_to_another_chunk: false,
                angles: 4,
            },
        ],
        min_height: 1.0,
        max_height: 2.0,
        set: 0,
    };
    // the victim still points back at p2 (id 2); joinPolygon must
    // retarget those to p1 (id 1)
    let victim = rect(6.0, 0.0, 8.0, 2.0, [2, 2, 0, 0]);
    gener.polygons = vec![east_run(2), west_side, victim];
    gener.bsp.push(BspNode {
        waypoint_index: 1,
        ..BspNode::default()
    });

    assert!(gener.join_polygon(0));

    // the winner grew: p1's three non-shared vertices + p2's three
    assert_eq!(gener.polygons[0].vertices.len(), 6);
    assert_eq!(gener.polygons[0].min_height, 0.0);
    assert_eq!(gener.polygons[0].max_height, 2.0);
    // the loser is empty and every pointer into it was retargeted
    assert!(gener.polygons[1].vertices.is_empty());
    assert_eq!(gener.polygons[2].vertices[0].adj_nav_poly, 1);
    assert_eq!(gener.polygons[2].vertices[1].adj_nav_poly, 1);
    // the bsp node that named the loser now names the winner
    assert_eq!(gener.bsp[0].waypoint_index, 0);
}

/// `processNode`'s even split: a node wider than
/// `EVEN_SPLIT_THRESHOLD` replaces the selected split with a balanced
/// one (`waypoint_generator.cpp:660-670`). The scene is a 30x3 strip
/// with an impassable gap column at x = 15, flooded from both sides.
#[test]
fn oversized_nodes_are_split_evenly() {
    let mut physics = FieldPhysics {
        slabs: vec![
            Slab(-100.0, -100.0, 14.6, 100.0, 0.0),
            Slab(15.4, -100.0, 100.0, 100.0, 0.0),
        ],
        max_step: 0.6,
    };
    let mut flood = grid(0.0, 30.0, 1.0);
    // shrink z to 3 cells so width 30 > height 3 picks normal 2
    flood.set_area(
        &Vector3::new(0.0, 0.0, 0.0),
        &Vector3::new(30.0, 0.0, 3.0),
        1.0,
    );
    let chunk = Bounds::everywhere();
    for s in &[seed(0.5, 10.0, 0.5), seed(16.5, 10.0, 0.5)] {
        flood.fill(s, &mut physics, &chunk, None);
    }
    flood.postfilter_add();
    flood.postfilter_remove();

    let mut gener = WaypointGenerator::new();
    gener.init(
        flood.xsize(),
        flood.zsize(),
        &Vector3::new(0.0, 0.0, 0.0),
        1.0,
    );
    for g in 0..MAX_HEIGHTS {
        gener.hgt_grids_mut()[g] = flood.hgt_grids()[g].to_vec();
        gener.adj_grids_mut()[g] = flood.adj_grids()[g].to_vec();
    }

    gener.generate();
    // the root ignored the gap-column suggestion and split evenly
    assert_eq!(gener.bsp[0].split_normal, 2);
    assert!((gener.bsp[0].split_offset - (-15.0)).abs() < 1e-4);
    assert!(gener.polygon_count() >= 2);

    gener.streamline();
    // the impassable strip at x = 15 never becomes waypoints (the dead
    // column leaves those nodes mostly impassable, like the slope's dead
    // column), so the polys stop at 14 and 16 and two sets form. Seeds
    // must sit within 1.5 of the polygon heights to count as frontier.
    gener.calculate_set_membership(&[seed(0.5, 1.0, 0.5), seed(16.5, 1.0, 0.5)], "wide");
    assert_eq!(gener.sets(), 2);
    assert_eq!(gener.polygon_count(), 2);
    assert_eq!(gener.poly_set(0), 1);
    assert_eq!(gener.poly_set(1), 2);
    // NavView::first_vertex participates in save-out merging; exercise
    // it here as no truth dump reads it.
    let fv = gener.first_vertex(0);
    let v0 = gener.vertex(0, 0);
    assert_eq!(fv, (v0.x, v0.z));
}

/// `averageHeight` with no populated level returns the zero pair
/// (`waypoint_generator.cpp:848-851`).
#[test]
fn average_height_of_an_empty_node_is_zero() {
    let mut gener = WaypointGenerator::new();
    gener.init(2, 2, &Vector3::new(0.0, 0.0, 0.0), 1.0);
    gener.bsp.push(BspNode::default());
    assert_eq!(gener.average_height(0), (0.0, 0));
}

// --- direct probes of the ordering ladders ------------------------------

/// The `PointDef`/`EdgeDef` ordering ladders and equality semantics
/// (`waypoint_generator.cpp:39-132`): coarse fudge equality, the
/// offset Greater arm, the max-height tiebreak and the id tiebreak.
#[test]
fn point_and_edge_orderings_match_the_reference_ladders() {
    let base = super::PointDef {
        angle: 1,
        offset: 2.0,
        t: 3.0,
        min_height: 0.0,
        max_height: 4.0,
    };
    // operator== is the exact cmp_ref equality
    assert!(base == base);
    assert!(base == super::PointDef { ..base });
    // outside the offset fudge, offset decides; Greater arm
    let lower = super::PointDef {
        offset: 1.0,
        ..base
    };
    assert!(base > lower);
    // within the fudge for offset and t, heights break the tie
    let taller = super::PointDef {
        max_height: 5.0,
        ..base
    };
    assert!(taller > base);

    let edge = super::EdgeDef {
        from: (0.0, 0.0),
        to: (1.0, 0.0),
        id: 1,
    };
    // positions only decide ==; id breaks < when positions tie
    assert!(edge == edge);
    assert!(edge == super::EdgeDef { ..edge });
    let higher_id = super::EdgeDef { id: 2, ..edge };
    assert!(edge != higher_id);
    assert!(edge < higher_id);
    assert_eq!(edge.partial_cmp(&higher_id), Some(std::cmp::Ordering::Less));

    // the reference keeps a Default next to the constructor
    let _ = WaypointGenerator::default();
}

/// `nodesSame` compares the edge pairs of two nodes: identical nodes
/// match, a node whose geometry lost an edge does not
/// (`waypoint_generator.cpp:425-468`).
#[test]
fn nodes_same_compares_edge_pairs() {
    let mut gener = WaypointGenerator::new();
    gener.init(4, 4, &Vector3::new(0.0, 0.0, 0.0), 1.0);
    gener.init_bsp();

    gener.bsp.push(gener.bsp[0].clone());
    assert!(gener.nodes_same(0, 1));

    // an all-zero border box has no edges at all: the pair existence
    // check fails for every angle
    gener.bsp[1] = BspNode {
        border_offset: [0.0; 8],
        ..gener.bsp[0].clone()
    };
    assert!(!gener.nodes_same(0, 1));
}

/// `splitNode`'s guard arms: equal-height vertical suggestions are
/// rejected untouched, diagonal cuts that collapse a side are caught
/// by the zero-size check, and a workable vertical suggestion walks
/// the front ceiling up until the split no longer disconnects points
/// (`waypoint_generator.cpp:992-1131`).
#[test]
fn split_node_rejects_bad_cuts_and_walks_the_front_ceiling() {
    let mut gener = WaypointGenerator::new();
    gener.init(8, 2, &Vector3::new(0.0, 0.0, 0.0), 1.0);
    // layer 1 carries the walkable heights (grid 0 keeps the init
    // zeroes, whose empty adjacency maps make every pass skip them):
    // a 0.5 rung at x = 3 linked +x into a 1.25 rung at x = 4
    for z in 0..2 {
        let hgt = gener.hgt_grids_mut();
        hgt[1][3 + 8 * z] = 0.5;
        hgt[1][4 + 8 * z] = 1.25;
        let adj = gener.adj_grids_mut();
        adj[1][3 + 8 * z].set_angle(2, 1);
        adj[1][4 + 8 * z].set_angle(6, 1);
    }
    gener.init_bsp();

    // equal heights: no vertical split, bsp untouched
    assert!(!gener.split_node(
        0,
        &SplitDef {
            normal: VERTICAL_SPLIT,
            value: 1.0,
            ph: [1.0, 1.0],
        },
        false,
    ));
    assert_eq!(gener.bsp.len(), 1);

    // a diagonal cut straight onto the far diagonal border: the front's
    // border pair sums to exactly zero
    assert!(!gener.split_node(
        0,
        &SplitDef {
            normal: 1,
            value: 1.0,
            ph: [7.0, 1.0],
        },
        false,
    ));
    assert_eq!(gener.bsp.len(), 1);

    // the real cut: at the midpoint 1.0 the x = 4 rung (1.25) is out of
    // the front's range and disconnects the x = 3 rung, so the ceiling
    // walk raises it until 1.25 fits
    assert!(gener.split_node(
        0,
        &SplitDef {
            normal: VERTICAL_SPLIT,
            value: 1.0,
            ph: [0.5, 1.5],
        },
        false,
    ));
    assert_eq!(gener.bsp.len(), 3);
    // the walk raised the ceiling past 1.25 until the x = 4 rung fit,
    // then reduce clamped the front's range onto the topmost rung; the
    // back keeps the raised floor and holds nothing below it
    assert_eq!(gener.bsp[1].min_height, 0.5);
    assert_eq!(gener.bsp[1].max_height, 1.25);
    assert!(gener.bsp[2].min_height > 1.25);
}

/// `refineNodeEdges`' degenerate-scan repair: a single-column scan is
/// widened so the node keeps nonzero size, in x and in z
/// (`waypoint_generator.cpp:1200-1214`).
#[test]
fn refine_node_edges_grows_degenerate_scans() {
    let mut gener = WaypointGenerator::new();
    gener.init(4, 4, &Vector3::new(0.0, 0.0, 0.0), 1.0);

    // node occupying only the x = 2 column, z in [0, 3]
    let mut column = BspNode {
        border_offset: [0.0; 8],
        min_height: 0.0,
        max_height: 0.0,
        ..BspNode::default()
    };
    column.border_offset[4] = 3.0;
    column.border_offset[2] = -2.0;
    column.border_offset[6] = 2.0;
    column.border_offset[1] = -1.4;
    column.border_offset[3] = 1.0;
    column.border_offset[5] = 10.0;
    column.border_offset[7] = 1.4;
    gener.refine_node_edges(&mut column);
    // min_x == max_x == 2 grew west by one cell (border = -min_x)
    assert!((column.border_offset[2] + 1.0).abs() < 1e-4);

    // node occupying only the (2, 0) point of a [0,3]x[0,3] box: the
    // x - z diagonal border cuts every z >= 1 point off
    let mut point_sized = BspNode {
        border_offset: [0.0; 8],
        min_height: 0.0,
        max_height: 0.0,
        ..BspNode::default()
    };
    point_sized.border_offset[4] = 3.0;
    point_sized.border_offset[2] = -2.0;
    point_sized.border_offset[6] = 2.0;
    point_sized.border_offset[1] = -1.4;
    point_sized.border_offset[3] = -1.2;
    point_sized.border_offset[5] = 10.0;
    point_sized.border_offset[7] = 1.4;
    gener.refine_node_edges(&mut point_sized);
    // min_z == max_z == 0 grew north by one cell
    assert!((point_sized.border_offset[4] - 1.0).abs() < 1e-4);

    // a node whose diagonal border sits past the last grid point: the
    // diagonal-only scan finds nothing, the scan collapses onto the
    // box origin and the zero-size guard grows it back
    let mut unreachable = BspNode {
        border_offset: [0.0; 8],
        min_height: 0.0,
        max_height: 0.0,
        ..BspNode::default()
    };
    unreachable.border_offset[4] = 1.0;
    unreachable.border_offset[6] = 7.0;
    // x + z >= 8.4 required to stay; the box tops out at x + z == 8
    unreachable.border_offset[1] = -8.5;
    unreachable.border_offset[3] = 100.0;
    unreachable.border_offset[5] = 100.0;
    unreachable.border_offset[7] = 100.0;
    gener.refine_node_edges(&mut unreachable);
    assert!((unreachable.border_offset[6] - 1.0).abs() < 1e-4);
    assert!((unreachable.border_offset[4] - 1.0).abs() < 1e-4);
}

/// `calcSplitValue` for vertical splits: a suggestion centred exactly
/// on the node's mid height has zero distance, which falls back to the
/// 0.1 floor (`waypoint_generator.cpp:949-956`).
#[test]
fn calc_split_value_floors_zero_distance_verticals() {
    let mut gener = WaypointGenerator::new();
    gener.init(2, 2, &Vector3::new(0.0, 0.0, 0.0), 1.0);
    let node = BspNode {
        min_height: 1.0,
        max_height: 2.0,
        ..BspNode::default()
    };
    let mut split = SplitDef {
        normal: VERTICAL_SPLIT,
        value: -1.0,
        ph: [1.0, 2.0],
    };
    gener.calc_split_value(&node, &mut split);
    assert!((split.value - 10.0).abs() < 1e-4);
}

/// `generateAdjacencies` skips edge matches whose polygon heights are
/// disjoint: the seam exists in x-z but the floors never overlap
/// (`waypoint_generator.cpp:1769-1771`).
#[test]
fn generate_adjacencies_rejects_height_disjoint_neighbours() {
    let mut gener = WaypointGenerator::new();
    let mut low = rect(0.0, 0.0, 2.0, 2.0, [0, 0, 0, 0]);
    low.min_height = 0.0;
    low.max_height = 1.0;
    let mut high = rect(2.0, 0.0, 4.0, 2.0, [0, 0, 0, 0]);
    high.min_height = 5.0;
    high.max_height = 6.0;
    gener.polygons = vec![low, high];

    gener.generate_adjacencies();

    // the shared seam x = 2 never links in either direction
    assert_eq!(gener.polygons[0].vertices[1].adj_nav_poly, 0);
    assert_eq!(gener.polygons[1].vertices[3].adj_nav_poly, 0);
}

/// `isConvexJoint`'s wrap-around and equal-angle arms
/// (`waypoint_generator.cpp:2064-2074`).
#[test]
fn is_convex_joint_matches_the_reference_predicate() {
    // a2 < a1 wraps: b past a1 or before a2 counts
    assert!(WaypointGenerator::is_convex_joint(2, 1, 0));
    assert!(!WaypointGenerator::is_convex_joint(2, 1, 1));
    // equal angles are never convex
    assert!(!WaypointGenerator::is_convex_joint(1, 1, 0));
    // straight ordering requires b between a1 and a2
    assert!(WaypointGenerator::is_convex_joint(1, 2, 1));
    assert!(!WaypointGenerator::is_convex_joint(1, 2, 2));
}

/// A waypoint node whose x-z box has zero depth emits no polygon: the
/// area precheck in `generatePolygons` drops it
/// (`waypoint_generator.cpp:1607-1611`).
#[test]
fn zero_depth_nodes_are_skipped_by_polygon_emission() {
    let mut gener = WaypointGenerator::new();
    gener.init(8, 1, &Vector3::new(0.0, 0.0, 0.0), 1.0);
    // one row of walkable cells linked along +x / -x (layer 1); only
    // on-grid angles are set, like the flood would
    for x in 0..8i32 {
        if x < 7 {
            gener.adj_grids_mut()[1][x as usize].set_angle(2, 1);
        }
        if x > 0 {
            gener.adj_grids_mut()[1][x as usize].set_angle(6, 1);
        }
    }

    gener.generate();

    assert_eq!(gener.polygon_count(), 0);
}

/// A height layer that is in range but has no adjacency at all — a
/// lone flooded cell cut off by an impassable moat — is skipped by the
/// node passes instead of blocking them
/// (`waypoint_generator.cpp:592-595` and `:1252-1255`).
#[test]
fn isolated_cells_are_skipped_by_the_node_passes() {
    let mut physics = FieldPhysics {
        slabs: vec![
            // ground ring around a moat at [3, 6) x [3, 6)
            Slab(-100.0, -100.0, 3.0, 100.0, 0.0),
            Slab(6.0, -100.0, 100.0, 100.0, 0.0),
            Slab(3.0, -100.0, 6.0, 3.0, 0.0),
            Slab(3.0, 6.0, 6.0, 100.0, 0.0),
            // a lone slab inside the moat, one cell wide
            Slab(4.0, 4.0, 5.0, 5.0, 0.0),
        ],
        max_step: 0.6,
    };
    let mut flood = grid(0.0, 8.0, 1.0);
    let chunk = Bounds::everywhere();
    for s in &[seed(0.5, 10.0, 0.5), seed(4.5, 10.0, 4.5)] {
        flood.fill(s, &mut physics, &chunk, None);
    }
    flood.postfilter_add();
    flood.postfilter_remove();

    let mut gener = WaypointGenerator::new();
    gener.init(
        flood.xsize(),
        flood.zsize(),
        &Vector3::new(0.0, 0.0, 0.0),
        1.0,
    );
    for g in 0..MAX_HEIGHTS {
        gener.hgt_grids_mut()[g] = flood.hgt_grids()[g].to_vec();
        gener.adj_grids_mut()[g] = flood.adj_grids()[g].to_vec();
    }

    gener.generate();
    gener.streamline();
    // the ground ring still becomes polygons; the moat and the lone
    // cell never do
    assert!(gener.polygon_count() >= 1);
}

/// `findDispoints` skips points outside the node's flags, layers with
/// no adjacency, and links whose neighbour leaves the node
/// (`waypoint_generator.cpp:896-917`).
#[test]
fn find_dispoints_skips_out_of_node_scans() {
    let mut gener = WaypointGenerator::new();
    gener.init(4, 4, &Vector3::new(0.0, 0.0, 0.0), 1.0);
    // layer 1: rungs at x = 1 linked +x (off the node, covered below);
    // layer 0: init zeroes with one +x adjacency so a scan reaches the
    // angle loop and finds the neighbour outside
    for z in 0..4usize {
        gener.hgt_grids_mut()[1][1 + 4 * z] = 0.5;
        gener.adj_grids_mut()[1][1 + 4 * z].set_angle(2, 1);
        if z >= 2 {
            gener.adj_grids_mut()[0][1 + 4 * z].set_angle(2, 1);
        }
    }

    // node rectangle x in [0, 1], z in [0, 3] with a corner cut that
    // leaves (0, 0), (1, 0) and (0, 1) outside the flags
    let mut node = BspNode {
        border_offset: [0.0; 8],
        min_height: 0.0,
        max_height: 0.0,
        ..BspNode::default()
    };
    node.border_offset[4] = 3.0;
    node.border_offset[6] = 1.0;
    node.border_offset[1] = -1.0;
    node.border_offset[3] = 100.0;
    node.border_offset[5] = 100.0;
    node.border_offset[7] = 100.0;
    node.update_point_in_node_flags();

    // every in-node point is either scanned-and-skipped or its link
    // leaves the node: no disconnect, and no panic on the neighbours
    assert!(!gener.find_dispoints(&node));
}

/// `calculateSetMembership` pops polygons already assigned by an
/// earlier stack entry without re-flooding them
/// (`waypoint_generator.cpp:2980-2985`).
#[test]
fn set_membership_revisits_queued_targets() {
    let mut gener = WaypointGenerator::new();
    gener.init(8, 8, &Vector3::new(0.0, 0.0, 0.0), 1.0);
    // two vertices of polygon 0 point at polygon 1, so the flood queue
    // holds it twice
    gener.polygons = vec![
        rect(0.0, 0.0, 2.0, 2.0, [2, 2, 0, 0]),
        rect(2.0, 0.0, 4.0, 2.0, [0, 0, 0, 0]),
    ];

    gener.calculate_set_membership(&[seed(1.0, 1.0, 1.0)], "dup");

    assert_eq!(gener.sets(), 1);
    assert_eq!(gener.polygons[0].set, 1);
    assert_eq!(gener.polygons[1].set, 1);
}

/// `doBestHorizontalSplit` with no acceptable candidate: the split is
/// abandoned and both children are flagged non-waypoints
/// (`waypoint_generator.cpp:780-797`).
#[test]
fn do_best_horizontal_split_without_candidates_quits() {
    let mut gener = WaypointGenerator::new();
    gener.init(4, 4, &Vector3::new(0.0, 0.0, 0.0), 1.0);
    gener.init_bsp();

    // init zeroes every grid: averageHeight counts nothing, so no
    // candidate split ever becomes acceptable. the caller has already
    // popped the index, so the stack arrives empty and stays empty.
    let mut stack = Vec::new();
    gener.do_best_horizontal_split(0, &mut stack);

    assert!(stack.is_empty());
    assert_eq!(gener.bsp.len(), 3);
    assert!(!gener.bsp[1].waypoint);
    assert!(!gener.bsp[2].waypoint);
}
