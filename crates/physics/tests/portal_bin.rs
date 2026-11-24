//! BW-10-03-07 integration: the reference-driven portal fixture
//! (`docs/review/fixtures/physics/bsp/gen_portal.log`) must replay
//! through the port bit for bit.
//!
//! The fixture drives the ORIGINAL el7 library end to end — the
//! `Portal` plane constructor, `setCollisionState`, the `PortalObstacle`
//! ctor/buildTriangles/collide object code from `libchunk.a`, and the
//! header-only `SweepParams`/`SweepShape`/`collideIntoObject`/`CollisionState`/
//! `CollisionCallback` — under a chunk mapping of scale 2 · rotY 90 ·
//! translate, so the world → chunk coordinate transform is genuinely
//! exercised. Four probe batteries (2 ray sets, 2 prism sweeps each
//! with hit and miss shapes) run on both sides of every portal state
//! switch: default open, closed with flags 0x2103, reopened, closed
//! with flags 0. The acceptance clause — 开关前后结果与基线一致 — is
//! pinned by states 0/2 answering identically and 1/3 identically
//! modulo the visited flags byte.

use foundation::math::{Matrix, PlaneEq, Vector3};
use physics::{
    CollisionCallback, CollisionInterface, CollisionObstacle, CollisionState, Portal,
    PortalObstacle, SweepParams, SweepShape, collide_into_object,
};

const FIXTURE_DIR: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../docs/review/fixtures/physics/bsp"
);

// ---------------------------------------------------------------------------
// bit helpers
// ---------------------------------------------------------------------------

fn b(f: f32) -> u32 {
    f.to_bits()
}

fn p(line: &str, idx: usize) -> u32 {
    u32::from_str_radix(line.split_whitespace().nth(idx).expect("field"), 16).expect("hex bits")
}

fn pf(line: &str, idx: usize) -> f32 {
    f32::from_bits(p(line, idx))
}

fn pdec(line: &str, idx: usize) -> i32 {
    line.split_whitespace()
        .nth(idx)
        .expect("field")
        .parse()
        .expect("int")
}

/// The truth printer walks a matrix as three axis rows plus the origin.
fn mat_bits(m: &Matrix) -> [u32; 12] {
    let mut out = [0u32; 12];
    for axis in 0..3 {
        let v = m.apply_to_unit_axis_vector(axis);
        out[axis * 3] = b(v.x);
        out[axis * 3 + 1] = b(v.y);
        out[axis * 3 + 2] = b(v.z);
    }
    let o = m.apply_to_origin();
    out[9] = b(o.x);
    out[10] = b(o.y);
    out[11] = b(o.z);
    out
}

fn assert_mat(m: &Matrix, line: &str, tag: &str) {
    let got = mat_bits(m);
    for (i, g) in got.iter().enumerate() {
        assert_eq!(
            *g,
            p(line, 1 + i),
            "{tag} component {i}: word {} of {line}",
            1 + i
        );
    }
}

fn assert_vec3(v: Vector3, line: &str, offset: usize, tag: &str) {
    assert_eq!(b(v.x), p(line, offset), "{tag}.x vs {line}");
    assert_eq!(b(v.y), p(line, offset + 1), "{tag}.y vs {line}");
    assert_eq!(b(v.z), p(line, offset + 2), "{tag}.z vs {line}");
}

// ---------------------------------------------------------------------------
// the truth callback: records every visit for the VST comparison
// ---------------------------------------------------------------------------

struct VisitRecord {
    transform: Matrix,
    transform_inverse: Matrix,
    triangle: physics::WorldTriangle,
    dist: f32,
}

struct TruthCallback {
    say: i32,
    visits: Vec<VisitRecord>,
}

impl TruthCallback {
    fn new(say: i32) -> Self {
        Self {
            say,
            visits: Vec::new(),
        }
    }
}

impl CollisionCallback for TruthCallback {
    fn do_visit(
        &mut self,
        obstacle: &CollisionObstacle,
        triangle: &physics::WorldTriangle,
        dist: f32,
    ) -> i32 {
        self.visits.push(VisitRecord {
            transform: obstacle.transform,
            transform_inverse: obstacle.transform_inverse,
            triangle: *triangle,
            dist,
        });
        self.say
    }
}

// ---------------------------------------------------------------------------
// the replay
// ---------------------------------------------------------------------------

#[test]
fn the_reference_portal_probes_answer_bit_for_bit_across_the_switch() {
    let log = std::fs::read_to_string(format!("{FIXTURE_DIR}/gen_portal.log")).expect("truth log");
    let lines: Vec<&str> = log.lines().collect();

    // ---- mapper: scale 2, rotY 90, translate (post-multiplied) ----
    let mut m_trans = Matrix::zero();
    m_trans.set_translate(10.0, 0.0, 5.0);
    let mut m_rot = Matrix::zero();
    // MATH_PI * 0.5f: the double product narrows to the float param
    m_rot.set_rotate_y((std::f64::consts::PI * 0.5) as f32);
    let mut m_scale = Matrix::zero();
    m_scale.set_scale(2.0, 2.0, 2.0);

    let mut chunk_xform = m_trans;
    chunk_xform.post_multiply(&m_rot);
    chunk_xform.post_multiply(&m_scale);

    let mpr_line = lines.iter().find(|l| l.starts_with("MPR ")).expect("MPR");
    assert_mat(&chunk_xform, mpr_line, "MPR");

    // The outside chunk's transform in the fixture equals the mapper
    // (its grid translate is zero at ctor time) — truth-pinned by CXT.
    let cxt_line = lines.iter().find(|l| l.starts_with("CXT ")).expect("CXT");
    assert_mat(&chunk_xform, cxt_line, "CXT");

    // ---- portals ----
    let pt1_line = lines
        .iter()
        .find(|l| l.starts_with("PT1 PLN"))
        .expect("PT1 PLN");
    assert_eq!(*pt1_line, "PT1 PLN 3f800000 00000000 00000000");
    let pt1_d = lines
        .iter()
        .find(|l| l.starts_with("PT1 PLD"))
        .expect("PT1 PLD");
    let mut portal_rect = Portal::from_plane(
        PlaneEq::new(Vector3::new(1.0, 0.0, 0.0), pf(pt1_d, 2)),
        Vector3::new(0.0, 0.0, 1.0),
        &[
            Vector3::new(-2.0, -1.0, 0.0),
            Vector3::new(2.0, -1.0, 0.0),
            Vector3::new(2.0, 3.0, 0.0),
            Vector3::new(-2.0, 3.0, 0.0),
        ],
        "",
    );
    check_portal_dump("PT1", &portal_rect, &lines);

    let pt2_line = lines
        .iter()
        .find(|l| l.starts_with("PT2 PLN"))
        .expect("PT2 PLN");
    assert_eq!(*pt2_line, "PT2 PLN 00000000 00000000 bf800000");
    let pt2_d = lines
        .iter()
        .find(|l| l.starts_with("PT2 PLD"))
        .expect("PT2 PLD");
    let mut portal_pen = Portal::from_plane(
        PlaneEq::new(Vector3::new(0.0, 0.0, -1.0), pf(pt2_d, 2)),
        Vector3::new(1.0, 0.0, 0.0),
        &[
            Vector3::new(0.0, -1.0, 0.0),
            Vector3::new(2.0, 0.0, 0.0),
            Vector3::new(1.5, 2.0, 0.0),
            Vector3::new(0.0, 2.5, 0.0),
            Vector3::new(-2.0, 1.0, 0.0),
        ],
        "",
    );
    check_portal_dump("PT2", &portal_pen, &lines);

    // ---- obstacles ----
    let obstacle_rect = PortalObstacle::new(&chunk_xform, &portal_rect);
    let obstacle_pen = PortalObstacle::new(&chunk_xform, &portal_pen);

    let obt = lines.iter().find(|l| l.starts_with("OBT ")).expect("OBT");
    assert_mat(&obstacle_rect.transform, obt, "OBT");
    let obi = lines.iter().find(|l| l.starts_with("OBI ")).expect("OBI");
    assert_mat(&obstacle_rect.transform_inverse, obi, "OBI");
    let obb_min = lines
        .iter()
        .find(|l| l.starts_with("OBBMIN"))
        .expect("OBBMIN");
    assert_vec3(obstacle_rect.bb.min, obb_min, 1, "OBBMIN");
    let obb_max = lines
        .iter()
        .find(|l| l.starts_with("OBBMAX"))
        .expect("OBBMAX");
    assert_vec3(obstacle_rect.bb.max, obb_max, 1, "OBBMAX");

    // round trip: chunk-local -> world -> obstacle-local, all through the
    // original-matrix-shaped port
    let rt1f = lines.iter().find(|l| l.starts_with("RT1F")).expect("RT1F");
    let world = chunk_xform.apply_point(&Vector3::new(-5.0, 0.0, 0.0));
    assert_vec3(world, rt1f, 1, "RT1F");
    let rt1b = lines.iter().find(|l| l.starts_with("RT1B")).expect("RT1B");
    assert_vec3(
        obstacle_rect.transform_inverse.apply_point(&world),
        rt1b,
        1,
        "RT1B",
    );
    let rt1s = lines.iter().find(|l| l.starts_with("RT1S")).expect("RT1S");
    assert_vec3(
        obstacle_rect
            .transform
            .apply_point(&Vector3::new(-5.0, 0.0, 0.0)),
        rt1s,
        1,
        "RT1S",
    );

    // ---- the state-switch batteries ----
    let mut i = 0usize;
    let mut batteries = 0usize;
    let mut ray_probes = 0usize;
    let mut prism_probes = 0usize;
    let mut vst_checks = 0usize;

    while i < lines.len() {
        let line = lines[i];
        if line.starts_with("PSTM rect close flags 2103") {
            portal_rect.set_collision_state(false, 0x2103);
            portal_pen.set_collision_state(false, 0x2103);
        } else if line == "PSTM rect open" {
            portal_rect.set_collision_state(true, 0);
            portal_pen.set_collision_state(true, 0);
        } else if line == "PSTM rect close flags 0" {
            portal_rect.set_collision_state(false, 0);
            portal_pen.set_collision_state(false, 0);
        } else if line.starts_with("STATE ") {
            batteries += 1;
        } else if line.starts_with("RAY ") && line.contains(" LOC ") {
            // RAY <tag> LOC <6> WRD <6> SAY <n>  ... later the RET line
            let tag = line.split_whitespace().nth(1).expect("tag");
            let local_s = Vector3::new(pf(line, 3), pf(line, 4), pf(line, 5));
            let local_e = Vector3::new(pf(line, 6), pf(line, 7), pf(line, 8));
            let world_s = chunk_xform.apply_point(&local_s);
            let world_e = chunk_xform.apply_point(&local_e);
            assert_vec3(world_s, line, 10, "RAY WRD src");
            assert_vec3(world_e, line, 13, "RAY WRD end");
            let say = pdec(line, 17);

            // collect the interleaved VST lines up to the RET
            let mut vst_lines = Vec::new();
            let mut ret_line = None;
            let mut j = i + 1;
            while j < lines.len() {
                if lines[j].starts_with("PRT VST ") {
                    vst_lines.push(lines[j]);
                } else if lines[j].starts_with(&format!("RAY {tag} RET")) {
                    ret_line = Some(lines[j]);
                    break;
                }
                j += 1;
            }
            let ret = ret_line.expect("RET line");

            let pen = tag == "R5" || tag == "R6";
            let (portal, obstacle, bb, inv): (
                &Portal,
                &dyn CollisionInterface,
                &foundation::math::BoundBox,
                &Matrix,
            ) = if pen {
                (
                    &portal_pen,
                    &obstacle_pen,
                    &obstacle_pen.bb,
                    &obstacle_pen.transform_inverse,
                )
            } else {
                (
                    &portal_rect,
                    &obstacle_rect,
                    &obstacle_rect.bb,
                    &obstacle_rect.transform_inverse,
                )
            };

            let mut cb = TruthCallback::new(say);
            let visits = {
                let mut cs = CollisionState::new(&mut cb);
                let start = SweepShape::Ray(world_s);
                let sp = SweepParams::new(&start, world_e);
                let stop = collide_into_object(&start, &sp, obstacle, portal, bb, inv, &mut cs);
                let dst = cs.dist;
                let st = cs.s_travel;
                let et = cs.e_travel;
                assert_eq!(if stop { 1 } else { 0 }, pdec(ret, 3), "{tag} RET vs {ret}");
                assert_eq!(b(dst), p(ret, 5), "{tag} DST vs {ret}");
                assert_eq!(b(st), p(ret, 7), "{tag} STR vs {ret}");
                assert_eq!(b(et), p(ret, 9), "{tag} ETR vs {ret}");
                cb.visits.len()
            };
            assert_eq!(visits as i32, pdec(ret, 11), "{tag} VIS vs {ret}");
            assert_eq!(vst_lines.len(), visits, "{tag} VST count");

            for (v, vl) in cb.visits.iter().zip(vst_lines.iter()) {
                // PRT VST M <12> MI <12> WT <9> FLG <8hex> DST <bits>
                let got = mat_bits(&v.transform);
                for (k, g) in got.iter().enumerate() {
                    assert_eq!(*g, p(vl, 3 + k), "{tag} VST M {k} vs {vl}");
                }
                let got = mat_bits(&v.transform_inverse);
                for (k, g) in got.iter().enumerate() {
                    assert_eq!(*g, p(vl, 16 + k), "{tag} VST MI {k} vs {vl}");
                }
                let t = v.triangle;
                for (k, vv) in [t.v0(), t.v1(), t.v2()].iter().enumerate() {
                    assert_vec3(*vv, vl, 29 + k * 3, &format!("{tag} VST WT{k}"));
                }
                assert_eq!(t.flags() as u32, p(vl, 39), "{tag} VST FLG vs {vl}");
                assert_eq!(b(v.dist), p(vl, 41), "{tag} VST DST vs {vl}");
            }
            vst_checks += vst_lines.len();
            ray_probes += 1;
            i = j;
        } else if line.starts_with("PRS ") && line.contains(" LOC ") {
            let tag = line.split_whitespace().nth(1).expect("tag");
            let local_a = Vector3::new(pf(line, 3), pf(line, 4), pf(line, 5));
            let local_b = Vector3::new(pf(line, 6), pf(line, 7), pf(line, 8));
            let local_c = Vector3::new(pf(line, 9), pf(line, 10), pf(line, 11));
            let local_end = Vector3::new(pf(line, 13), pf(line, 14), pf(line, 15));
            let wa = chunk_xform.apply_point(&local_a);
            let wb = chunk_xform.apply_point(&local_b);
            let wc = chunk_xform.apply_point(&local_c);
            let wend = chunk_xform.apply_point(&local_end);
            assert_vec3(wa, line, 17, "PRS WRD a");
            assert_vec3(wb, line, 20, "PRS WRD b");
            assert_vec3(wc, line, 23, "PRS WRD c");
            assert_vec3(wend, line, 27, "PRS WRD end");
            let say = pdec(line, 31);

            let mut vst_lines = Vec::new();
            let mut ret_line = None;
            let mut j = i + 1;
            while j < lines.len() {
                if lines[j].starts_with("PRT VST ") {
                    vst_lines.push(lines[j]);
                } else if lines[j].starts_with(&format!("PRS {tag} RET")) {
                    ret_line = Some(lines[j]);
                    break;
                }
                j += 1;
            }
            let ret = ret_line.expect("PRS RET line");

            let mut cb = TruthCallback::new(say);
            let visits = {
                let mut cs = CollisionState::new(&mut cb);
                let start = SweepShape::Prism(physics::WorldTriangle::new(wa, wb, wc, 0));
                let sp = SweepParams::new(&start, wend);
                let stop = collide_into_object(
                    &start,
                    &sp,
                    &obstacle_rect,
                    &portal_rect,
                    &obstacle_rect.bb,
                    &obstacle_rect.transform_inverse,
                    &mut cs,
                );
                assert_eq!(if stop { 1 } else { 0 }, pdec(ret, 3), "{tag} RET vs {ret}");
                assert_eq!(b(cs.dist), p(ret, 5), "{tag} DST vs {ret}");
                assert_eq!(b(cs.s_travel), p(ret, 7), "{tag} STR vs {ret}");
                assert_eq!(b(cs.e_travel), p(ret, 9), "{tag} ETR vs {ret}");
                cb.visits.len()
            };
            assert_eq!(visits as i32, pdec(ret, 11), "{tag} VIS vs {ret}");
            assert_eq!(vst_lines.len(), visits, "{tag} VST count");
            for (v, vl) in cb.visits.iter().zip(vst_lines.iter()) {
                let got = mat_bits(&v.transform);
                for (k, g) in got.iter().enumerate() {
                    assert_eq!(*g, p(vl, 3 + k), "{tag} VST M {k} vs {vl}");
                }
                let got = mat_bits(&v.transform_inverse);
                for (k, g) in got.iter().enumerate() {
                    assert_eq!(*g, p(vl, 16 + k), "{tag} VST MI {k} vs {vl}");
                }
                let t = v.triangle;
                for (k, vv) in [t.v0(), t.v1(), t.v2()].iter().enumerate() {
                    assert_vec3(*vv, vl, 29 + k * 3, &format!("{tag} VST WT{k}"));
                }
                assert_eq!(t.flags() as u32, p(vl, 39), "{tag} VST FLG vs {vl}");
                assert_eq!(b(v.dist), p(vl, 41), "{tag} VST DST vs {vl}");
            }
            vst_checks += vst_lines.len();
            prism_probes += 1;
            i = j;
        } else if line == "RESULT OK" {
            break;
        }
        i += 1;
    }

    // acceptance: four batteries (two probes per portal state per side of
    // the switch), 11 ray + 2 prism probes per battery, plus the switch
    // markers and the tail
    assert_eq!(batteries, 4, "four STATE batteries");
    assert_eq!(ray_probes, 36, "9 ray probes per battery");
    assert_eq!(prism_probes, 8, "2 prism probes per battery");
    // the closed states visit 6 ray + 1 prism probes each, twice
    assert_eq!(vst_checks, 14, "visits recorded across the closed states");
    assert!(lines[i..].contains(&"RESULT OK"), "the truth run completed");
}

/// Compare one portal's dump block (PLN..PPM) against the ported ctor.
fn check_portal_dump(tag: &str, portal: &Portal, lines: &[&str]) {
    let find = |prefix: String| {
        lines
            .iter()
            .find(|l| l.starts_with(&prefix))
            .unwrap_or_else(|| panic!("{prefix}"))
    };

    let pln = find(format!("{tag} PLN"));
    assert_vec3(*portal.plane.normal(), pln, 2, "PLN");
    let pld = find(format!("{tag} PLD"));
    assert_eq!(b(portal.plane.d()), p(pld, 2), "{tag} PLD vs {pld}");
    let uax = find(format!("{tag} UAX"));
    assert_vec3(portal.u_axis, uax, 2, "UAX");
    let vax = find(format!("{tag} VAX"));
    assert_vec3(portal.v_axis, vax, 2, "VAX");
    let org = find(format!("{tag} ORG"));
    assert_vec3(portal.origin, org, 2, "ORG");

    for (k, pt) in portal.points.iter().enumerate() {
        let line = find(format!("{tag} PT {k}"));
        assert_eq!(b(pt.x), p(line, 3), "{tag} PT{k}.x vs {line}");
        assert_eq!(b(pt.y), p(line, 4), "{tag} PT{k}.y vs {line}");
    }
    assert_eq!(portal.points.len(), {
        let mut n = 0;
        let prefix = format!("{tag} PT ");
        for l in lines {
            if l.starts_with(&prefix) {
                n += 1;
            }
        }
        n
    });

    let ppm = find(format!("{tag} PPM"));
    assert_eq!(
        portal.permissive as i32,
        pdec(ppm, 2),
        "{tag} PPM (default open)"
    );
}
