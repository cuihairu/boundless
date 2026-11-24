//! Acceptance artifacts for BW-10-05-02: the two scenes of the
//! reference harness `docs/review/fixtures/waypoint/gen_flood.cpp` (a
//! 0.5-step slope and an indoor/outdoor shell with a bridge stacking a
//! second height layer) re-run through the Rust port, dumped in the
//! harness's exact `FILL/SCENE/H/A/B/TGA` format and compared line by
//! line against the committed truth log `gen_flood.log` produced by
//! compiling the original `waypoint_flood.cpp`.

use super::tests::*;
use super::*;
use foundation::math::vector::Vector3;

fn slab_scene() -> FieldPhysics {
    FieldPhysics {
        slabs: Vec::new(),
        max_step: 0.6,
    }
}

/// `slopeScene`: eight 0.5-step slabs covering `x` in `[0,8)`.
fn slope_scene() -> FieldPhysics {
    let mut physics = slab_scene();
    for i in 0..8 {
        let i = i as f32;
        physics.slabs.push(Slab(i, -100.0, i + 1.0, 100.0, 0.5 * i));
    }
    physics
}

/// `shellScene`: outdoor ground strips, a sealed room at floor 0.5
/// (unclimbable 3.0 walls), and an indoor bridge at 1.5 that stacks
/// second height layers over the floor.
fn shell_scene() -> FieldPhysics {
    let mut physics = slab_scene();
    physics.slabs.push(Slab(-100.0, -100.0, 0.5, 100.0, 0.0));
    physics.slabs.push(Slab(6.5, -100.0, 100.0, 100.0, 0.0));
    physics.slabs.push(Slab(0.5, -100.0, 100.0, 0.5, 0.0));
    physics.slabs.push(Slab(0.5, 6.5, 100.0, 100.0, 0.0));
    physics.slabs.push(Slab(1.5, 1.5, 5.5, 5.5, 0.5));
    physics.slabs.push(Slab(0.5, 0.5, 1.5, 6.5, 3.0));
    physics.slabs.push(Slab(0.5, 0.5, 6.5, 1.5, 3.0));
    physics.slabs.push(Slab(0.5, 5.5, 6.5, 6.5, 3.0));
    physics.slabs.push(Slab(5.5, 0.5, 6.5, 6.5, 3.0));
    physics.slabs.push(Slab(1.5, 2.5, 4.5, 4.5, 1.5));
    physics
}

/// The chunk clip of `runScene`: shell cells own `x <= 3.5`, so the
/// east column leaves boundary points behind.
fn clipped_chunk() -> Bounds {
    Bounds {
        owns_box: (-100.0, -100.0, 3.5, 100.0),
        space: (-100.0, 100.0, -100.0, 100.0),
    }
}

/// One `runScene` pass: seeds, postfilters and the harness dump text.
fn run_scene(
    name: &str,
    physics: &mut FieldPhysics,
    chunk: &Bounds,
    seeds: &[Vector3],
    tga_path: &str,
    out: &mut String,
) {
    let mut flood = grid(0.0, 8.0, 1.0);
    let mut total = 0;
    for (i, s) in seeds.iter().enumerate() {
        let c = flood.fill(s, physics, chunk, None);
        out.push_str(&format!("FILL {i} count {c}\n"));
        total += c;
    }

    flood.postfilter_add();
    flood.postfilter_remove();

    out.push_str(&format!("SCENE {name} count {total}\n"));
    let cells = (flood.xsize() * flood.zsize()) as usize;
    for g in 0..MAX_HEIGHTS {
        for i in 0..cells {
            let h = flood.hgt_grids()[g][i];
            if h > -9999.0 {
                out.push_str(&format!("H {g} {i} {h:.1}\n"));
            }
        }
    }
    for g in 0..MAX_HEIGHTS {
        for i in 0..cells {
            let all = flood.adj_grids()[g][i].all;
            for a in 0..8usize {
                let v = (all >> (a << 2)) & 0xF;
                if v != 0 {
                    out.push_str(&format!("A {g} {i} {a} {v}\n"));
                }
            }
        }
    }
    for (index, grid_id) in flood.boundary_set() {
        out.push_str(&format!("B {index} {grid_id}\n"));
    }

    flood.write_tga(tga_path);
    let bytes = std::fs::read(tga_path).expect("tga written");
    let mut hash = 0xcbf29ce484222325u64;
    for b in &bytes {
        hash ^= u64::from(*b);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    out.push_str(&format!(
        "TGA {name} size {} fnv {hash:016x}\n",
        bytes.len()
    ));
}

#[test]
fn the_reference_harness_scenes_reproduce_the_truth_log() {
    let mut out = String::new();

    let tga = std::env::temp_dir().join("bw10_05_02_slope.tga");
    let mut physics = slope_scene();
    let chunk = Bounds::everywhere();
    run_scene(
        "slope",
        &mut physics,
        &chunk,
        &[seed(0.5, 10.0, 0.5)],
        tga.to_str().unwrap(),
        &mut out,
    );

    let tga = std::env::temp_dir().join("bw10_05_02_shell.tga");
    let mut physics = shell_scene();
    let chunk = clipped_chunk();
    run_scene(
        "shell",
        &mut physics,
        &chunk,
        &[
            seed(0.5, 10.0, 0.5),
            seed(3.0, 10.0, 3.5),
            seed(2.5, 1.0, 4.5),
        ],
        tga.to_str().unwrap(),
        &mut out,
    );

    let truth = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../docs/review/fixtures/waypoint/gen_flood.log"
    ));
    if out != truth {
        for (n, (ours, theirs)) in out.lines().zip(truth.lines()).enumerate() {
            if ours != theirs {
                panic!(
                    "first mismatch at log line {}:\n  ours:   {ours}\n  truth:  {theirs}",
                    n + 1
                );
            }
        }
        panic!(
            "line counts differ: ours {} truth {}",
            out.lines().count(),
            truth.lines().count()
        );
    }
}
