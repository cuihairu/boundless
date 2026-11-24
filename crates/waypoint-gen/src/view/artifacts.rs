//! Acceptance artifacts for BW-10-05-04 (persistence slice): the test
//! scene of the reference harness
//! `docs/review/fixtures/waypoint/gen_view.cpp` — 5 polygons over 2 nav
//! poly sets covering every `asBinary` adjacency arm — re-run through
//! the Rust port, dumped in the harness's exact `VIEW/ASBINARY/R/P/V`
//! format plus the `saveOut` passes (`CDATA1` fresh, `CDATA2` merged
//! over three authored old records), and compared line by line against
//! the committed truth log `gen_view.log` produced by compiling the
//! original `waypoint_view.cpp` unmodified.

use super::*;
use std::fmt::Write as _;

/// One authored polygon: set, heights, world-space vertices carrying the
/// RAW adjacency the reference's `getVertex` returns.
struct HarnessPoly {
    set: i32,
    min_height: f32,
    max_height: f32,
    verts: Vec<NavVertex>,
}

/// The harness view: grid min (10, 0, 20), resolution 0.5 (the vertices
/// are already scaled by `VertexRef::getVertex`).
struct HarnessView {
    polys: Vec<HarnessPoly>,
}

impl NavView for HarnessView {
    fn polygon_count(&self) -> i32 {
        self.polys.len() as i32
    }
    fn girth(&self, set: i32) -> f32 {
        if set == 0 { 0.0 } else { 1.2 }
    }
    fn poly_set(&self, poly: i32) -> i32 {
        self.polys[poly as usize].set
    }
    fn min_height(&self, poly: i32) -> f32 {
        self.polys[poly as usize].min_height
    }
    fn max_height(&self, poly: i32) -> f32 {
        self.polys[poly as usize].max_height
    }
    fn vertex_count(&self, poly: i32) -> i32 {
        self.polys[poly as usize].verts.len() as i32
    }
    fn first_vertex(&self, poly: i32) -> (f32, f32) {
        let v = &self.polys[poly as usize].verts[0];
        (v.x, v.z)
    }
    fn vertex(&self, poly: i32, vertex: i32) -> NavVertex {
        self.polys[poly as usize].verts[vertex as usize]
    }
}

/// The harness scene: 5 polygons, 2 sets, every adjacency arm
/// (in-set remap, cross-set phantom 0, chunk border 65535, vista
/// `~-a`, out-of-range passthrough, self adjacency).
fn harness_view() -> HarnessView {
    HarnessView {
        polys: vec![
            HarnessPoly {
                set: 0,
                min_height: 0.5,
                max_height: 0.5,
                verts: vec![
                    NavVertex {
                        x: 10.5,
                        z: 20.5,
                        adj_nav_poly: 1,
                        adj_to_another_chunk: false,
                    },
                    NavVertex {
                        x: 11.0,
                        z: 20.5,
                        adj_nav_poly: 3,
                        adj_to_another_chunk: false,
                    },
                    NavVertex {
                        x: 11.0,
                        z: 21.0,
                        adj_nav_poly: 0,
                        adj_to_another_chunk: true,
                    },
                    NavVertex {
                        x: 10.5,
                        z: 21.0,
                        adj_nav_poly: -1,
                        adj_to_another_chunk: false,
                    },
                ],
            },
            HarnessPoly {
                set: 0,
                min_height: 1.5,
                max_height: 1.5,
                verts: vec![
                    NavVertex {
                        x: 11.0,
                        z: 20.5,
                        adj_nav_poly: 2,
                        adj_to_another_chunk: false,
                    },
                    NavVertex {
                        x: 11.5,
                        z: 20.5,
                        adj_nav_poly: 7,
                        adj_to_another_chunk: false,
                    },
                    NavVertex {
                        x: 11.5,
                        z: 21.0,
                        adj_nav_poly: -5,
                        adj_to_another_chunk: false,
                    },
                ],
            },
            HarnessPoly {
                set: 0,
                min_height: 2.5,
                max_height: 2.5,
                verts: vec![NavVertex {
                    x: 11.5,
                    z: 21.0,
                    adj_nav_poly: 0,
                    adj_to_another_chunk: true,
                }],
            },
            HarnessPoly {
                set: 1,
                min_height: 0.5,
                max_height: 0.5,
                verts: vec![
                    NavVertex {
                        x: 10.5,
                        z: 22.0,
                        adj_nav_poly: 4,
                        adj_to_another_chunk: false,
                    },
                    NavVertex {
                        x: 11.0,
                        z: 22.0,
                        adj_nav_poly: 1,
                        adj_to_another_chunk: false,
                    },
                    NavVertex {
                        x: 11.0,
                        z: 22.5,
                        adj_nav_poly: -2,
                        adj_to_another_chunk: false,
                    },
                    NavVertex {
                        x: 10.5,
                        z: 22.5,
                        adj_nav_poly: 5,
                        adj_to_another_chunk: false,
                    },
                ],
            },
            HarnessPoly {
                set: 1,
                min_height: 1.0,
                max_height: 1.0,
                verts: vec![
                    NavVertex {
                        x: 10.5,
                        z: 22.5,
                        adj_nav_poly: -1,
                        adj_to_another_chunk: false,
                    },
                    NavVertex {
                        x: 11.0,
                        z: 22.5,
                        adj_nav_poly: 1,
                        adj_to_another_chunk: false,
                    },
                ],
            },
        ],
    }
}

fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf29ce484222325;
    for b in bytes {
        hash ^= u64::from(*b);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

/// `dumpBinary`: decode a record stream and emit the harness text.
fn dump_binary(what: &str, bytes: &[u8], out: &mut String) {
    let _ = writeln!(
        out,
        "{what} bytes {} fnv {:016x}",
        bytes.len(),
        fnv1a64(bytes)
    );
    // byte-exact cross-check: 16 bytes per HX line
    for (i, chunk) in bytes.chunks(16).enumerate() {
        let mut hex = String::with_capacity(32);
        for b in chunk {
            let _ = write!(hex, "{:02x}", b);
        }
        let _ = writeln!(out, "HX {:04x} {}", i * 16, hex);
    }
    let mut off = 0usize;
    let mut record = 0;
    while off < bytes.len() {
        let version = i32::from_le_bytes(bytes[off..off + 4].try_into().unwrap());
        let girth = f32::from_le_bytes(bytes[off + 4..off + 8].try_into().unwrap());
        let nav_poly_elts = i32::from_le_bytes(bytes[off + 8..off + 12].try_into().unwrap());
        let nav_poly_edges = i32::from_le_bytes(bytes[off + 12..off + 16].try_into().unwrap());
        let _ = writeln!(
            out,
            "R {record} version {version} girth {girth:.1} nwp {nav_poly_elts} ne {nav_poly_edges}"
        );
        // [header][all ctrl][all data] — decode in that order
        let ctrl = off + 16;
        let data = ctrl + (nav_poly_elts as usize) * NAV_POLY_ELT_SIZE;
        for p in 0..nav_poly_elts as usize {
            let c = ctrl + p * NAV_POLY_ELT_SIZE;
            let min_height = f32::from_le_bytes(bytes[c..c + 4].try_into().unwrap());
            let max_height = f32::from_le_bytes(bytes[c + 4..c + 8].try_into().unwrap());
            let vc = i32::from_le_bytes(bytes[c + 8..c + 12].try_into().unwrap());
            let _ = writeln!(
                out,
                "  P {p} minh {min_height:.1} maxh {max_height:.1} vc {vc}"
            );
        }
        let end = data + (nav_poly_edges as usize) * NAV_POLY_EDGE_ELT_SIZE;
        for (v, e) in (data..end).step_by(NAV_POLY_EDGE_ELT_SIZE).enumerate() {
            let x = f32::from_le_bytes(bytes[e..e + 4].try_into().unwrap());
            let z = f32::from_le_bytes(bytes[e + 4..e + 8].try_into().unwrap());
            let adj = i32::from_le_bytes(bytes[e + 8..e + 12].try_into().unwrap());
            writeln!(out, "  V {v} x {x:.1} z {z:.1} adj {adj}").unwrap();
        }
        off = end;
        record += 1;
    }
}

/// `makeOldNavmesh`: three authored old records for the saveOut merge —
/// the 0.8 record hits the default girth (dropped), 1.5 and 0.6 are
/// kept in their old order after the fresh records.
fn make_old_navmesh() -> Vec<u8> {
    let mut old = Vec::new();
    // edge adjacency is a raw int, so the body is built per element
    let mut record = |girth: f32, nwp: i32, ne: i32, body: &[u8]| {
        old.extend_from_slice(&0i32.to_le_bytes());
        old.extend_from_slice(&girth.to_le_bytes());
        old.extend_from_slice(&nwp.to_le_bytes());
        old.extend_from_slice(&ne.to_le_bytes());
        old.extend_from_slice(body);
    };
    let poly = |minh: f32, maxh: f32, vc: i32| -> Vec<u8> {
        let mut v = minh.to_le_bytes().to_vec();
        v.extend_from_slice(&maxh.to_le_bytes());
        v.extend_from_slice(&vc.to_le_bytes());
        v
    };
    let edge = |x: f32, z: f32, adj: i32| -> Vec<u8> {
        let mut v = x.to_le_bytes().to_vec();
        v.extend_from_slice(&z.to_le_bytes());
        v.extend_from_slice(&adj.to_le_bytes());
        v
    };
    let mut a = poly(9.0, 9.0, 1);
    a.extend_from_slice(&edge(11.0, 21.0, 0));
    record(0.8, 1, 1, &a);
    record(1.5, 0, 0, &[]);
    record(0.6, 0, 1, &edge(12.0, 22.0, -4));
    old
}

#[test]
fn the_reference_harness_scene_reproduces_the_truth_log() {
    let view = harness_view();
    let mut out = String::new();

    let _ = writeln!(out, "VIEW polys {}", view.polygon_count());
    let fresh = as_binary(&view, 0.8);
    dump_binary("ASBINARY", &fresh, &mut out);

    // saveOut pass 1: no cdata yet -> created fresh with one child
    let cdata1 = save_out_records(&fresh, None, 0.8, true);
    let _ = writeln!(out, "SAVEOUT 1 cdata 1 children");
    dump_binary("CDATA1", &cdata1, &mut out);
    let _ = writeln!(
        out,
        "CHUNK sects 0 resource gen_view.cdata/worldNavmesh boundary 0 navpolyset 0"
    );

    // saveOut pass 2: old navmesh in place, removeAllOld false -> the
    // default-girth old record is superseded, 1.5 and 0.6 kept in order
    let old = make_old_navmesh();
    let cdata2 = save_out_records(&fresh, Some(&old), 0.8, false);
    dump_binary("CDATA2", &cdata2, &mut out);

    let truth = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../docs/review/fixtures/waypoint/gen_view.log"
    ));
    assert_matches_truth(&out, truth);
}

/// The truth-log comparator: per-line equality with a first-mismatch
/// report, then a length report.
fn assert_matches_truth(ours: &str, truth: &str) {
    if ours != truth {
        for (n, (ours, theirs)) in ours.lines().zip(truth.lines()).enumerate() {
            if ours != theirs {
                panic!(
                    "first mismatch at log line {}:\n  ours:   {ours}\n  truth:  {theirs}",
                    n + 1
                );
            }
        }
        panic!(
            "line counts differ: ours {} truth {}",
            ours.lines().count(),
            truth.lines().count()
        );
    }
}

#[test]
#[should_panic(expected = "first mismatch at log line 2")]
fn the_view_truth_comparator_reports_the_first_mismatching_line() {
    assert_matches_truth("A\nB\n", "A\nC\n");
}

#[test]
#[should_panic(expected = "line counts differ: ours 2 truth 1")]
fn the_view_truth_comparator_reports_line_count_differences() {
    assert_matches_truth("A\nB\n", "A\n");
}

#[test]
fn save_out_merges_kept_records_after_the_fresh_stream() {
    let view = harness_view();
    let fresh = as_binary(&view, 0.8);
    let old = make_old_navmesh();

    let merged = save_out_records(&fresh, Some(&old), 0.8, false);
    // fresh 260 + kept (1.5 empty 16 + 0.6 one edge 28)
    assert_eq!(merged.len(), 260 + 16 + 28);
    assert_eq!(&merged[..fresh.len()], &fresh[..]);
    // the kept records follow in their old order
    // (headers are [version][girth][nwp][ne] — 1.5 at 260, 0.6 at 276)
    assert_eq!(merged[260 + 4..260 + 8], 1.5f32.to_le_bytes());
    assert_eq!(merged[276 + 4..276 + 8], 0.6f32.to_le_bytes());
    assert_eq!(merged[276 + 8..276 + 12], 0i32.to_le_bytes()); // nwp
    assert_eq!(merged[276 + 12..276 + 16], 1i32.to_le_bytes()); // ne
}

#[test]
fn save_out_with_only_default_girth_old_records_returns_new_alone() {
    let view = harness_view();
    let fresh = as_binary(&view, 0.8);

    let mut all_default = Vec::new();
    all_default.extend_from_slice(&0i32.to_le_bytes());
    all_default.extend_from_slice(&0.8f32.to_le_bytes());
    all_default.extend_from_slice(&0i32.to_le_bytes());
    all_default.extend_from_slice(&0i32.to_le_bytes());

    // keepSize == 0 -> the fresh stream stands alone
    assert_eq!(
        save_out_records(&fresh, Some(&all_default), 0.8, false),
        fresh
    );
    // removeAllOld -> old navmesh ignored even when it has kept records
    assert_eq!(
        save_out_records(&fresh, Some(&make_old_navmesh()), 0.8, true),
        fresh
    );
}
